use super::dto::{self, OrganizationDto, UserDto};
use super::jwt::{self, Claims};
use crate::modules::auth::session::{SessionId, SESSION_DAYS_DURATION};
use anyhow::{Context, Result};
use bcrypt::{hash, verify, DEFAULT_COST};
use chrono::{Duration, Utc};
use ipnetwork::IpNetwork;
use migration::Expr;
use rand_chacha::ChaCha8Rng;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, DbErr, EntityTrait, QueryFilter, Set,
    TransactionTrait, TryIntoModel,
};
use shared::constants::Permission;
use shared::entity::{access_level, organization, session, user};
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
    db: DatabaseConnection,
}

impl AuthService {
    pub fn new(db: DatabaseConnection, rng: ChaCha8Rng) -> Self {
        AuthService {
            db,
            rng: Arc::new(Mutex::new(rng)),
        }
    }

    /// generates a new session token and creates a new session record on the DB for the user
    pub async fn new_session(
        &self,
        user_identifier: i32,
        client_ip: IpAddr,
        client_user_agent: String,
    ) -> Result<SessionId> {
        let ses_token = SessionId::generate_new(&mut self.rng.lock().unwrap());

        let new_session = session::ActiveModel {
            ip: Set(IpNetwork::from(client_ip).to_string()),
            user_agent: Set(client_user_agent),
            expires_at: Set(Utc::now() + Duration::days(SESSION_DAYS_DURATION)),
            user_id: Set(user_identifier),
            session_token: Set(ses_token.into_database_value()),
            ..Default::default()
        };

        new_session.insert(&self.db).await?;

        Ok(ses_token)
    }

    /// lists all sessions belonging to a user
    pub async fn get_active_user_sessions(&self, user_id: i32) -> Result<Vec<session::Model>> {
        let sessions = session::Entity::find()
            .filter(session::Column::ExpiresAt.gt(Utc::now()))
            .filter(session::Column::UserId.eq(user_id))
            .all(&self.db)
            .await?;

        Ok(sessions)
    }

    /// deletes a session by its token
    pub async fn delete_session(&self, session_id: &SessionId) -> Result<()> {
        session::Entity::delete_many()
            .filter(session::Column::SessionToken.eq(session_id.into_database_value()))
            .exec(&self.db)
            .await?;

        Ok(())
    }

    /// deletes a session by its public ID
    pub async fn delete_session_by_public_id(&self, public_id: i32) -> Result<()> {
        session::Entity::delete_many()
            .filter(session::Column::PublicId.eq(public_id))
            .exec(&self.db)
            .await?;

        Ok(())
    }

    /// gets the user from the session token if the session is not expired
    pub async fn get_user_from_session_id(
        &self,
        session_id: SessionId,
    ) -> Result<Option<UserDtoEntities>> {
        let result = user::Entity::find()
            .inner_join(session::Entity)
            .filter(session::Column::ExpiresAt.gt(Utc::now()))
            .filter(session::Column::SessionToken.eq(session_id.into_database_value()))
            .find_also_related(organization::Entity)
            .one(&self.db)
            .await?;

        if let Some((user, organization)) = result {
            let access_level = access_level::Entity::find_by_id(user.access_level_id)
                .one(&self.db)
                .await?
                .context("access level not found")?;

            return Ok(Some((user, access_level, organization)));
        }

        Ok(None)
    }

    /// finds a user from email and plain text password, verifying the password
    pub async fn get_user_from_credentials(
        &self,
        user_email: String,
        user_password: String,
    ) -> Result<dto::UserDto, UserFromCredentialsError> {
        let result = user::Entity::find()
            .filter(user::Column::Email.eq(user_email))
            .find_also_related(organization::Entity)
            .one(&self.db)
            .await
            .or(Err(UserFromCredentialsError::InternalError))?;

        match result {
            Some((user, organization)) => {
                let access_level = access_level::Entity::find_by_id(user.access_level_id)
                    .one(&self.db)
                    .await
                    .or(Err(UserFromCredentialsError::InternalError))?
                    .ok_or(UserFromCredentialsError::NotFound)?;

                let pass_is_valid = verify(user_password, &user.password)
                    .or(Err(UserFromCredentialsError::InternalError))?;

                if !pass_is_valid {
                    return Err(UserFromCredentialsError::InvalidPassword);
                }

                Ok(UserDto::from((user, access_level, organization)))
            }
            None => Err(UserFromCredentialsError::NotFound),
        }
    }

    /// checks if a email is in use by a organization or a user
    pub async fn check_email_in_use(&self, email: &str) -> Result<bool> {
        let org = organization::Entity::find()
            .filter(organization::Column::BillingEmail.eq(email))
            .one(&self.db)
            .await?;

        if org.is_some() {
            return Ok(true);
        }

        let user = user::Entity::find()
            .filter(user::Column::Email.eq(email))
            .one(&self.db)
            .await?;

        Ok(user.is_some())
    }

    pub async fn get_user_id_by_username(&self, username: &str) -> Result<Option<i32>> {
        let user_id = user::Entity::find()
            .filter(user::Column::Username.eq(username))
            .one(&self.db)
            .await?
            .map(|u| u.id);

        Ok(user_id)
    }

    pub async fn gen_short_lived_token_for_user(&self, user_id: i32) -> Result<String> {
        let mut claims = Claims::default();

        claims.set_expiration_in(Duration::seconds(20));
        claims.aud = format!("user:{}", user_id);
        claims.sub = String::from("user short lived token");

        let token = jwt::encode(&claims)?;

        Ok(token)
    }

    pub fn get_user_id_from_token_aud(&self, aud: String) -> Result<i32> {
        let n = aud
            .strip_prefix("user:")
            .context("invalid token aud, user prefix not found")?;

        n.parse::<i32>().context("user token is not a valid int")
    }

    pub async fn gen_and_set_user_reset_password_token(&self, user_id: i32) -> Result<String> {
        let mut claims = Claims::default();

        claims.set_expiration_in(Duration::minutes(15));
        claims.aud = format!("user:{}", user_id);
        claims.sub = String::from("restore password token");

        let token = jwt::encode(&claims)?;

        user::Entity::update_many()
            .col_expr(user::Column::ResetPasswordToken, Expr::value(&token))
            .filter(user::Column::Id.eq(user_id))
            .exec(&self.db)
            .await?;

        Ok(token)
    }

    pub async fn gen_and_set_user_confirm_email_token(&self, user_id: i32) -> Result<String> {
        let mut claims = Claims::default();

        claims.set_expiration_in(Duration::hours(8));
        claims.aud = format!("user:{}", user_id);
        claims.sub = String::from("confirm email address token");

        let token = jwt::encode(&claims)?;

        user::Entity::update_many()
            .col_expr(user::Column::ConfirmEmailToken, Expr::value(&token))
            .filter(user::Column::Id.eq(user_id))
            .exec(&self.db)
            .await?;

        Ok(token)
    }

    pub async fn gen_and_set_org_confirm_email_token(&self, org_id: i32) -> Result<String> {
        let mut claims = Claims::default();

        claims.set_expiration_in(Duration::hours(8));
        claims.aud = format!("organization:{}", org_id);
        claims.sub = String::from("confirm email address token");

        let token = jwt::encode(&claims)?;

        organization::Entity::update_many()
            .col_expr(
                organization::Column::ConfirmBillingEmailToken,
                Expr::value(&token),
            )
            .filter(organization::Column::Id.eq(org_id))
            .exec(&self.db)
            .await?;

        Ok(token)
    }

    /// creates a new user and his organization, as well as a root access level for said org
    pub async fn register_user_and_organization(
        &self,
        dto: dto::RegisterOrganization,
    ) -> Result<dto::UserDto> {
        let password_hash = hash(dto.password, DEFAULT_COST)?;

        let user_dto = self
            .db
            .transaction::<_, UserDto, DbErr>(|tx| {
                Box::pin(async move {
                    let organization = organization::ActiveModel {
                        name: Set(dto.username.clone()),
                        blocked: Set(false),
                        billing_email: Set(dto.email.clone()),
                        billing_email_verified: Set(false),
                        ..Default::default()
                    }
                    .save(tx)
                    .await?
                    .try_into_model()?;

                    let access_level = access_level::ActiveModel {
                        name: Set(String::from("admin")),
                        is_fixed: Set(true),
                        description: Set(String::from("root access level")),
                        permissions: Set(Permission::to_string_vec()),
                        organization_id: Set(Some(organization.id)),
                        ..Default::default()
                    }
                    .save(tx)
                    .await?
                    .try_into_model()?;

                    let user = user::ActiveModel {
                        email: Set(dto.email),
                        username: Set(dto.username),
                        password: Set(password_hash),
                        email_verified: Set(false),
                        organization_id: Set(Some(organization.id)),
                        access_level_id: Set(access_level.id),
                        ..Default::default()
                    }
                    .save(tx)
                    .await?
                    .try_into_model()?;

                    let mut org: organization::ActiveModel = organization.clone().into();

                    org.owner_id = Set(Some(user.id));
                    org.update(tx).await?;

                    Ok(UserDto::from((user, access_level, Some(organization))))
                })
            })
            .await?;

        Ok(user_dto)
    }
}

/// tuple with relevant relationships to create a user dto
pub type UserDtoEntities = (
    user::Model,
    access_level::Model,
    Option<organization::Model>,
);

impl From<UserDtoEntities> for UserDto {
    fn from(m: UserDtoEntities) -> Self {
        let (user, access_level, org) = m;

        Self {
            id: user.id,
            created_at: user.created_at,
            username: user.username,
            email: user.email,
            email_verified: user.email_verified,
            profile_picture: user.profile_picture,
            description: user.description,
            organization: org.map(OrganizationDto::from),
            access_level: Into::into(access_level),
        }
    }
}
