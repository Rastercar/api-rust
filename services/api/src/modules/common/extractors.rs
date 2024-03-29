use crate::{
    database::error::DbError,
    modules::{auth::middleware::RequestUser, common::responses::SimpleError},
    server::controller::AppState,
};
use axum::{
    async_trait,
    extract::{rejection::JsonRejection, FromRequest, FromRequestParts, Path, Query},
    http::{request::Parts, Request, StatusCode},
    Json,
};
use axum_typed_multipart::{BaseMultipart, TypedMultipartError};
use sea_orm::DatabaseConnection;
use serde::de::DeserializeOwned;
use shared::entity::traits::QueryableByIdAndOrgId;
use validator::Validate;

/// Wrapper struct that extracts from the request query exactly `axum::Query<T>`
/// but also requires T to impl `Validate`, if validation fails a bad request code
/// and simple error is returned
#[derive(Clone, Copy)]
pub struct ValidatedQuery<T>(pub T);

#[async_trait]
impl<S, T> FromRequestParts<S> for ValidatedQuery<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Validate,
{
    type Rejection = (StatusCode, SimpleError);

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match Query::<T>::from_request_parts(parts, state).await {
            Ok(payload) => match payload.validate() {
                Ok(_) => Ok(ValidatedQuery(payload.0)),
                Err(e) => Err((StatusCode::BAD_REQUEST, SimpleError::from(e))),
            },
            Err(rejection) => Err((rejection.status(), SimpleError::from(rejection.to_string()))),
        }
    }
}

/// Wrapper struct that extracts the request body as json exactly as `axum::Json<T>`
/// but also requires T to impl `Validate`, if validation fails a bad request code
/// and simple error is returned
#[derive(Clone, Copy)]
pub struct ValidatedJson<T>(pub T);

#[async_trait]
impl<S, T> FromRequest<S> for ValidatedJson<T>
where
    Json<T>: FromRequest<S, Rejection = JsonRejection>,
    T: Validate,
    S: Send + Sync,
{
    type Rejection = (StatusCode, SimpleError);

    async fn from_request(
        req: Request<axum::body::Body>,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(payload) => match payload.validate() {
                Ok(_) => Ok(ValidatedJson(payload.0)),
                Err(e) => Err((StatusCode::BAD_REQUEST, SimpleError::from(e))),
            },
            Err(rejection) => Err((rejection.status(), SimpleError::from(rejection.to_string()))),
        }
    }
}

/// Wrapper struct that extracts the request body from `axum_typed_multipart::TryFromMultipart`
/// but also requires T to impl `Validate`, if validation fails a bad request code and simple
/// error is returned
#[derive(Clone, Copy)]
pub struct ValidatedMultipart<T>(pub T);

#[async_trait]
impl<S, T> FromRequest<S> for ValidatedMultipart<T>
where
    BaseMultipart<T, TypedMultipartError>: FromRequest<S, Rejection = TypedMultipartError>,
    T: Validate,
    S: Send + Sync,
{
    type Rejection = (StatusCode, SimpleError);

    async fn from_request(
        req: Request<axum::body::Body>,
        state: &S,
    ) -> Result<Self, Self::Rejection> {
        match BaseMultipart::<T, TypedMultipartError>::from_request(req, state).await {
            Ok(payload) => match payload.data.validate() {
                Ok(_) => Ok(ValidatedMultipart(payload.data)),
                Err(e) => Err((StatusCode::BAD_REQUEST, SimpleError::from(e))),
            },
            Err(rejection) => Err((
                StatusCode::BAD_REQUEST,
                SimpleError::from(rejection.to_string()),
            )),
        }
    }
}

/// Extracts the organization id of the request user, failing with
/// `(StatusCode::BAD_REQUEST, SimpleError::from("route only accessible to organization bound users"))`
/// if the request user is not bound to a organization.
///
/// this requires the `RequestUser` extension to be available.
#[derive(Clone, Copy)]
pub struct OrganizationId(pub i32);

#[async_trait]
impl<S> FromRequestParts<S> for OrganizationId
where
    S: Send + Sync,
{
    type Rejection = (StatusCode, SimpleError);

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let err = (
            StatusCode::FORBIDDEN,
            SimpleError::from("endpoint only for org bound users"),
        );

        if let Some(req_user) = parts.extensions.get::<RequestUser>() {
            let org_id = req_user.get_org_id().ok_or(err)?;

            return Ok(OrganizationId(org_id));
        }

        Err(err)
    }
}

/// Helper to get a DB connection from the state
pub struct DbConnection(pub DatabaseConnection);

#[async_trait]
impl FromRequestParts<AppState> for DbConnection {
    type Rejection = (http::StatusCode, SimpleError);

    async fn from_request_parts(_: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        Ok(DbConnection(state.db.clone()))
    }
}

/// Path extractor that fetches a entity by its ID on a endpoint path parameter
/// and the OrganizationId of the request user, returns a 404 response if the
/// entity is not found
#[derive(Clone, Copy)]
pub struct OrgBoundEntityFromPathId<T: QueryableByIdAndOrgId>(pub T::Model);

#[async_trait]
impl<T> FromRequestParts<AppState> for OrgBoundEntityFromPathId<T>
where
    T: QueryableByIdAndOrgId,
{
    type Rejection = (http::StatusCode, SimpleError);

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let org_id = OrganizationId::from_request_parts(parts, state).await?;

        let id: Path<i32> = Path::from_request_parts(parts, state).await.map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                SimpleError::from("failed to get entity id from path"),
            )
        })?;

        let entity = T::find_by_id_and_org_id(id.0, org_id.0, &state.db)
            .await
            .map_err(DbError::from)?
            .ok_or((StatusCode::NOT_FOUND, SimpleError::entity_not_found()))?;

        Ok(OrgBoundEntityFromPathId(entity))
    }
}
