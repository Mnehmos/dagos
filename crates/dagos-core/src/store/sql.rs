//! SQL column conversions for domain types.
//!
//! Values are validated again on the way out of the database, so a corrupted row surfaces as an
//! error instead of an invalid domain value.

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::domain::{
    Classification, ContextSource, ConversationId, EdgeId, EdgeType, ErrorCode, EventId, ModelId,
    NodeId, NodeType, ProjectId, ProviderId, RunId, RunStatus, Timestamp,
};

macro_rules! text_column {
    ($($ty:ty => $parse:expr;)+) => {$(
        impl ToSql for $ty {
            fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
                Ok(ToSqlOutput::from(self.as_str()))
            }
        }

        impl FromSql for $ty {
            fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
                let parse: fn(&str) -> Result<$ty, _> = $parse;
                parse(value.as_str()?).map_err(|error| FromSqlError::Other(Box::new(error)))
            }
        }
    )+};
}

text_column! {
    ProjectId => |text| ProjectId::parse(text);
    ConversationId => |text| ConversationId::parse(text);
    NodeId => |text| NodeId::parse(text);
    EdgeId => |text| EdgeId::parse(text);
    RunId => |text| RunId::parse(text);
    EventId => |text| EventId::parse(text);
    ProviderId => |text| ProviderId::parse(text);
    ModelId => |text| ModelId::parse(text);
    NodeType => |text| text.parse();
    EdgeType => |text| text.parse();
    RunStatus => |text| text.parse();
    ErrorCode => |text| text.parse();
    Classification => |text| text.parse();
    ContextSource => |text| text.parse();
}

impl ToSql for Timestamp {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.to_string()))
    }
}

impl FromSql for Timestamp {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        Timestamp::parse(value.as_str()?).map_err(|error| FromSqlError::Other(Box::new(error)))
    }
}

/// A JSON-encoded TEXT column.
pub(super) struct Json<T>(pub T);

impl<T: Serialize> ToSql for Json<T> {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        let text = serde_json::to_string(&self.0)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        Ok(ToSqlOutput::from(text))
    }
}

impl<T: DeserializeOwned> FromSql for Json<T> {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        serde_json::from_str(value.as_str()?)
            .map(Json)
            .map_err(|error| FromSqlError::Other(Box::new(error)))
    }
}
