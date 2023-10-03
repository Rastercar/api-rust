use super::constants::Permission;
use super::dto::{self, OrganizationDto};
use super::jwt::{self, Claims};
use crate::database::models;
use crate::database::schema::{access_level, organization, session, user};
use crate::modules::auth::session::{SessionToken, SESSION_DAYS_DURATION};
use anyhow::Result;
use bcrypt::{hash, verify, DEFAULT_COST};
use chrono::{Duration, Utc};
use diesel::prelude::*;
use diesel_async::scoped_futures::ScopedFutureExt;
use diesel_async::{
    pooled_connection::deadpool::Pool, AsyncConnection, AsyncPgConnection, RunQueryDsl,
};
use ipnetwork::IpNetwork;
use rand_chacha::ChaCha8Rng;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

pub enum UserFromCredentialsError {
    NotFound,
    InternalError,
    InvalidPassword,
}

#[derive(Clone)]
pub struct AuthService {
    rng: Arc<Mutex<ChaCha8Rng>>,
    db_conn_pool: Pool<AsyncPgConnection>,
}

pub fn new_auth_service(db_conn_pool: Pool<AsyncPgConnection>, rng: ChaCha8Rng) -> AuthService {
    AuthService {
        db_conn_pool,
        rng: Arc::new(Mutex::new(rng)),
    }
}

impl AuthService {
    /// generates a new session token and creates a new session record on the DB for the user
    pub async fn new_session(
        &self,
        user_identifier: i32,
        client_ip: IpAddr,
        client_user_agent: String,
    ) -> Result<SessionToken> {
        use crate::database::schema::session::dsl::*;

        let conn = &mut self.db_conn_pool.get().await?;

        let ses_token = SessionToken::generate_new(&mut self.rng.lock().unwrap());

        diesel::insert_into(session)
            .values((
                ip.eq(IpNetwork::from(client_ip)),
                user_agent.eq(client_user_agent),
                expires_at.eq(Utc::now() + chrono::Duration::days(SESSION_DAYS_DURATION)),
                user_id.eq(user_identifier),
                session_token.eq(ses_token.into_database_value()),
            ))
            .get_result::<models::Session>(conn)
            .await?;

        Ok(ses_token)
    }

    /// deletes a session by its token
    pub async fn delete_session(&self, token: SessionToken) -> Result<()> {
        use crate::database::schema::session::dsl::*;

        let conn = &mut self.db_conn_pool.get().await?;

        let delete_query = session.filter(session_token.eq(token.into_database_value()));
        diesel::delete(delete_query).execute(conn).await?;

        Ok(())
    }

    /// gets the user from the session token if the session is not expired
    pub async fn get_user_from_session_token(
        &self,
        token: SessionToken,
    ) -> Result<Option<dto::UserDto>> {
        let conn = &mut self.db_conn_pool.get().await?;

        let user_with_org_and_access_level = user::table
            .inner_join(session::table)
            .inner_join(access_level::table)
            .left_join(organization::table)
            .filter(session::dsl::session_token.eq(token.into_database_value()))
            .filter(session::dsl::expires_at.gt(Utc::now()))
            .select((
                models::User::as_select(),
                models::AccessLevel::as_select(),
                Option::<models::Organization>::as_select(),
            ))
            .first::<(
                models::User,
                models::AccessLevel,
                Option<models::Organization>,
            )>(conn)
            .await
            .optional()?;

        match user_with_org_and_access_level {
            None => Ok(None),
            Some(x) => {
                // type hints for shitty LSP
                let (usr, access_level, usr_org) = x as (
                    models::User,
                    models::AccessLevel,
                    Option<models::Organization>,
                );

                Ok(Some(dto::UserDto {
                    id: usr.id,
                    created_at: usr.created_at,
                    updated_at: usr.updated_at,
                    username: usr.username,
                    email: usr.email,
                    email_verified: usr.email_verified,
                    profile_picture: usr.profile_picture,
                    description: usr.description,
                    organization: usr_org.map(|u| OrganizationDto::from(u)),
                    access_level: Into::into(access_level),
                }))
            }
        }
    }

    /// finds a user from email and plain text password, verifying the password
    pub async fn get_user_from_credentials(
        &self,
        user_email: String,
        user_password: String,
    ) -> Result<models::User, UserFromCredentialsError> {
        use crate::database::schema::user::dsl::*;

        let conn = &mut self
            .db_conn_pool
            .get()
            .await
            .or(Err(UserFromCredentialsError::InternalError))?;

        let user_model: Option<models::User> = user
            .filter(email.eq(&user_email))
            .first(conn)
            .await
            .optional()
            .or(Err(UserFromCredentialsError::InternalError))?;

        match user_model {
            Some(usr) => {
                let pass_is_valid = verify(user_password, &usr.password)
                    .or(Err(UserFromCredentialsError::InternalError))?;

                if pass_is_valid {
                    Ok(usr)
                } else {
                    Err(UserFromCredentialsError::InvalidPassword)
                }
            }
            None => return Err(UserFromCredentialsError::NotFound),
        }
    }

    /// checks if a email is in use by a organization or a user
    pub async fn check_email_in_use(&self, email: &str) -> Result<bool> {
        let conn = &mut self.db_conn_pool.get().await?;

        let organization_id: Option<i32> = organization::dsl::organization
            .select(organization::dsl::id)
            .filter(organization::dsl::billing_email.eq(&email))
            .first(conn)
            .await
            .optional()?;

        if organization_id.is_some() {
            return Ok(true);
        }

        let user_id: Option<i32> = user::dsl::user
            .select(user::dsl::id)
            .filter(user::dsl::email.eq(&email))
            .first(conn)
            .await
            .optional()?;

        Ok(user_id.is_some())
    }

    pub async fn get_user_id_by_username(&self, username: &str) -> Result<Option<i32>> {
        let conn = &mut self.db_conn_pool.get().await?;

        let user_id: Option<i32> = user::dsl::user
            .select(user::dsl::id)
            .filter(user::dsl::username.eq(&username))
            .first(conn)
            .await
            .optional()?;

        Ok(user_id)
    }

    pub async fn gen_and_set_user_reset_password_token(&self, user_id: i32) -> Result<String> {
        use crate::database::schema::user::dsl::*;

        let now = Utc::now();

        let exp = (now + Duration::hours(8)).timestamp() as usize;
        let iat = now.timestamp() as usize;

        let token = jwt::encode(&Claims {
            exp,
            iat,
            aud: format!("user:{}", user_id),
            iss: String::from("rastercar API"),
            sub: String::from("restore password token"),
        })?;

        let conn = &mut self.db_conn_pool.get().await?;

        diesel::update(user)
            .filter(id.eq(user_id))
            .set(reset_password_token.eq(&token))
            .execute(conn)
            .await?;

        Ok(token)
    }

    /// creates a new user and his organization, as well as a root access level for said org
    pub async fn register_user_and_organization(
        &self,
        dto: dto::RegisterOrganization,
    ) -> Result<models::User> {
        let conn = &mut self.db_conn_pool.get().await?;

        let created_user = conn
            .transaction::<_, anyhow::Error, _>(|conn| {
                async move {
                    let created_organization = diesel::insert_into(organization::dsl::organization)
                        .values((
                            organization::dsl::name.eq(&dto.username),
                            organization::dsl::blocked.eq(false),
                            organization::dsl::billing_email.eq(&dto.email),
                            organization::dsl::billing_email_verified.eq(false),
                        ))
                        .get_result::<models::Organization>(conn)
                        .await?;

                    let created_access_level = diesel::insert_into(access_level::dsl::access_level)
                        .values((
                            access_level::dsl::name.eq("admin"),
                            access_level::dsl::is_fixed.eq(true),
                            access_level::dsl::description.eq("root access level"),
                            access_level::dsl::organization_id.eq(created_organization.id),
                            access_level::dsl::permissions.eq(Permission::to_string_vec()),
                        ))
                        .get_result::<models::AccessLevel>(conn)
                        .await?;

                    let created_user = diesel::insert_into(user::dsl::user)
                        .values((
                            user::dsl::email.eq(dto.email),
                            user::dsl::username.eq(dto.username),
                            user::dsl::password.eq(hash(dto.password, DEFAULT_COST)?),
                            user::dsl::email_verified.eq(false),
                            user::dsl::organization_id.eq(created_organization.id),
                            user::dsl::access_level_id.eq(created_access_level.id),
                        ))
                        .get_result::<models::User>(conn)
                        .await?;

                    diesel::update(organization::dsl::organization)
                        .filter(organization::dsl::id.eq(created_organization.id))
                        .set(organization::dsl::owner_id.eq(created_user.id))
                        .execute(conn)
                        .await?;

                    Ok(created_user)
                }
                .scope_boxed()
            })
            .await?;

        Ok(created_user)
    }
}
