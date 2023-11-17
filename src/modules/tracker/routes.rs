use super::dto::CreateTrackerDto;
use crate::{
    database::{
        error::DbError,
        models::VehicleTracker,
        pagination::PaginationResult,
        schema::{vehicle, vehicle_tracker},
    },
    modules::{
        auth::{self, constants::Permission, middleware::AclLayer},
        common::{
            dto::Pagination,
            extractors::{DbConnection, OrganizationId, ValidatedJson, ValidatedQuery},
            responses::{internal_error_response, SimpleError},
        },
    },
    server::controller::AppState,
};
use axum::{
    routing::{get, post},
    Json, Router,
};
use diesel::prelude::*;
use diesel_async::RunQueryDsl;
use futures_util::Future;
use http::StatusCode;

pub fn create_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/", post(create_tracker))
        .layer(AclLayer::new(vec![Permission::CreateTracker]))
        .route("/", get(list_trackers))
        .layer(axum::middleware::from_fn_with_state(
            state,
            auth::middleware::require_user,
        ))
}

/// Creates a new tracker
///
/// Required permissions: CREATE_TRACKER
#[utoipa::path(
    post,
    path = "/tracker",
    tag = "tracker",
    security(("session_id" = [])),
    request_body = CreateTrackerDto,
    responses(
        (
            status = OK,
            description = "the created tracker",
            content_type = "application/json",
            body = VehicleTracker,
        ),
        (
            status = UNAUTHORIZED,
            description = "expired or invalid token",
            body = SimpleError,
        ),
        (
            status = BAD_REQUEST,
            description = "invalid dto error message / IMEI_IN_USE",
            body = SimpleError,
        ),
    ),
)]
pub async fn create_tracker(
    OrganizationId(org_id): OrganizationId,
    DbConnection(mut conn): DbConnection,
    ValidatedJson(dto): ValidatedJson<CreateTrackerDto>,
) -> Result<Json<VehicleTracker>, (StatusCode, SimpleError)> {
    if let Some(vehicle_id) = dto.vehicle_id {
        let count: i64 = vehicle::dsl::vehicle
            .filter(vehicle::dsl::id.eq(vehicle_id))
            .filter(vehicle::dsl::organization_id.eq(org_id))
            .count()
            .get_result(&mut conn)
            .await
            .or(Err(internal_error_response()))?;

        if count != 1 {
            return Err((
                StatusCode::BAD_REQUEST,
                SimpleError::from(format!(
                    "vehicle: {} not found for org {}",
                    vehicle_id, org_id
                )),
            ));
        }

        let trackers_on_vehicle_cnt: i64 = vehicle_tracker::dsl::vehicle_tracker
            .filter(vehicle_tracker::dsl::vehicle_id.eq(vehicle_id))
            .count()
            .get_result(&mut conn)
            .await
            .or(Err(internal_error_response()))?;

        if trackers_on_vehicle_cnt > 0 {
            return Err((
                StatusCode::BAD_REQUEST,
                SimpleError::from(format!(
                    "vehicle: {} already has a tracker installed",
                    vehicle_id
                )),
            ));
        }
    }

    let created_tracker = diesel::insert_into(vehicle_tracker::dsl::vehicle_tracker)
        .values((
            vehicle_tracker::dsl::imei.eq(dto.imei),
            vehicle_tracker::dsl::model.eq(dto.model),
            vehicle_tracker::dsl::vehicle_id.eq(dto.vehicle_id),
            vehicle_tracker::dsl::organization_id.eq(org_id),
        ))
        .get_result::<VehicleTracker>(&mut conn)
        .await
        .map_err(|e| DbError::from(e))?;

    Ok(Json(created_tracker))
}

/// Lists the trackers that belong to the same org as the request user
#[utoipa::path(
    get,
    path = "/tracker",
    tag = "tracker",
    security(("session_id" = [])),
    params(
        Pagination
    ),
    responses(
        (
            status = OK,
            description = "paginated list of trackers",
            content_type = "application/json",
            body = PaginatedVehicleTracker,
        ),
        (
            status = UNAUTHORIZED,
            description = "expired or invalid token",
            body = SimpleError,
        ),
    ),
)]
pub async fn list_trackers(
    ValidatedQuery(p): ValidatedQuery<Pagination>,
    OrganizationId(org_id): OrganizationId,
    conn: DbConnection,
) -> Result<Json<PaginationResult<VehicleTracker>>, (StatusCode, SimpleError)> {
    // TODO: ver esse erro.
    // https://github.com/rust-lang/rust/issues/102211
    //
    // ocorre somemte com load_with_pagination
    //
    // por causa do
    // U: std::marker::Send
    //
    // see:
    // https://github.com/rust-lang/rust/issues/102211#issuecomment-1513931928
    let f = xd(conn);
    let fut = assert_send(f);
    let result = fut.await.or(Err(internal_error_response()))?;

    Ok(Json(result))
}

async fn xd(
    DbConnection(mut conn): DbConnection,
) -> Result<PaginationResult<VehicleTracker>, (StatusCode, SimpleError)> {
    use crate::database::pagination::*;

    let mut query = vehicle_tracker::dsl::vehicle_tracker.into_boxed();

    query = query.filter(vehicle_tracker::dsl::vehicle_id.eq(1));

    let result = query
        .order(vehicle_tracker::id.asc())
        .filter(vehicle_tracker::dsl::organization_id.eq(1))
        .select(VehicleTracker::as_select())
        .paginate(1 as i64)
        .per_page(1 as i64)
        .load_with_pagination::<VehicleTracker>(&mut conn)
        .await
        .or(Err(internal_error_response()))?;

    Ok(result)
}

// TODO:
fn assert_send<'u, R>(
    fut: impl 'u + Send + Future<Output = R>,
) -> impl 'u + Send + Future<Output = R> {
    fut
}
