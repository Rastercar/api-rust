use crate::database::models::VehicleTracker;
use diesel::pg::Pg;
use diesel::prelude::*;
use diesel::query_builder::*;
use diesel::sql_types::BigInt;
use diesel_async::{methods::LoadQuery, AsyncPgConnection, RunQueryDsl};
use serde::Serialize;
use utoipa::ToSchema;

const DEFAULT_PER_PAGE: i64 = 10;

/// A paginable query
///
/// types who implement this trait represent a SQL query that can be paginated
pub trait Paginate: Sized {
    /// Applies pagination to a query (self)
    fn paginate(self, page: i64) -> Paginated<Self>;
}

impl<T> Paginate for T {
    fn paginate(self, page: i64) -> Paginated<Self> {
        Paginated {
            page,
            query: self,
            per_page: DEFAULT_PER_PAGE,
            offset: (page - 1) * DEFAULT_PER_PAGE,
        }
    }
}

#[derive(Clone, Copy, QueryId)]
pub struct Paginated<T> {
    /// The query to be executed
    query: T,

    /// The page to fetch records from
    page: i64,

    /// amount of items to bring per page
    per_page: i64,

    /// Offset to fetch records from, result of: `(page - 1) * per_page`
    offset: i64,
}

/// Pagination metadata of a executed query
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[aliases(PaginatedVehicleTracker = PaginationResult<VehicleTracker>)]
pub struct PaginationResult<T> {
    /// Page number
    ///
    /// this is used to determine the offset used in the query
    page: i64,

    /// Offset used in the paginated query
    ///
    /// result of: `(page - 1) * records_per_page`
    offset: i64,

    /// Amount of total records available for the given query
    page_count: i64,

    /// Amount of records per page
    page_size: i64,

    /// Records from the query
    records: Vec<T>,
}

impl<'a, T: 'a> Paginated<T> {
    /// Sets the items per page of the pagination
    pub fn per_page(self, per_page: i64) -> Self {
        Paginated {
            per_page,
            offset: (self.page - 1) * per_page,
            ..self
        }
    }

    /// Executes the query, applying limit / offset pagination,
    /// returning the records and pagination metadata
    pub async fn load_with_pagination<U>(
        self,
        conn: &mut AsyncPgConnection,
    ) -> QueryResult<PaginationResult<U>>
    where
        Self: LoadQuery<'a, AsyncPgConnection, (U, i64)>,
        U: std::marker::Send,
    {
        let per_page = self.per_page;
        let page = self.page;
        let offset = self.offset;

        let results = self.load::<(U, i64)>(conn).await?;

        let total = results.get(0).map(|x| x.1).unwrap_or(0);

        let records: Vec<U> = results.into_iter().map(|x| x.0).collect();

        Ok(PaginationResult {
            page: page,
            offset: offset,
            page_count: (total as f64 / per_page as f64).ceil() as i64,
            page_size: per_page,
            records,
        })
    }
}

impl<T: Query> Query for Paginated<T> {
    type SqlType = (T::SqlType, BigInt);
}

impl<T> QueryFragment<Pg> for Paginated<T>
where
    T: QueryFragment<Pg>,
{
    fn walk_ast<'b>(&'b self, mut out: AstPass<'_, 'b, Pg>) -> QueryResult<()> {
        out.push_sql("SELECT *, COUNT(*) OVER () FROM (");

        self.query.walk_ast(out.reborrow())?;

        out.push_sql(") t LIMIT ");

        out.push_bind_param::<BigInt, _>(&self.per_page)?;

        out.push_sql(" OFFSET ");

        out.push_bind_param::<BigInt, _>(&self.offset)?;

        Ok(())
    }
}
