//! One PostgreSQL connection for both reference data and mutable game state.
use crate::{pickle::Value as V, state::Result};
use bytes::BytesMut;
use std::{error::Error, time::Duration};
use tokio_postgres::{
    Client, Config, NoTls,
    types::{FromSql, IsNull, ToSql, Type, to_sql_checked},
};

pub struct Connection {
    client: Client,
    runtime: tokio::runtime::Runtime,
}
impl Connection {
    pub fn open(url: &str) -> Result<Self> {
        let mut config: Config = url.parse().map_err(|_| "invalid DATABASE_URL")?;
        config.connect_timeout(Duration::from_secs(10));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        let (client, connection) = runtime.block_on(config.connect(NoTls))?;
        runtime.spawn(async move {
            if let Err(e) = connection.await {
                eprintln!("PostgreSQL connection closed: {e}");
            }
        });
        let db = Self { client, runtime };
        db.batch("SET statement_timeout='30s'; SET lock_timeout='10s'; SET idle_in_transaction_session_timeout='60s'")?;
        Ok(db)
    }
    pub fn batch(&self, sql: &str) -> Result<()> {
        self.runtime.block_on(self.client.batch_execute(sql))?;
        Ok(())
    }
    pub fn query(&self, sql: &str, args: &[V]) -> Result<Vec<V>> {
        let params = args.iter().map(Param).collect::<Vec<_>>();
        let params = params
            .iter()
            .map(|p| p as &(dyn ToSql + Sync))
            .collect::<Vec<_>>();
        let rows = self.runtime.block_on(self.client.query(sql, &params))?;
        rows.into_iter()
            .map(|row| {
                let mut fields = Vec::new();
                for (i, col) in row.columns().iter().enumerate() {
                    fields.push((col.name().into(), row.try_get::<_, Field>(i)?.0));
                }
                Ok(V::Dict(fields))
            })
            .collect()
    }
    pub fn one(&self, sql: &str, args: &[V]) -> Result<V> {
        Ok(self.query(sql, args)?.into_iter().next().unwrap_or(V::None))
    }
    pub fn exec(&self, sql: &str, args: &[V]) -> Result<usize> {
        let params = args.iter().map(Param).collect::<Vec<_>>();
        let params = params
            .iter()
            .map(|p| p as &(dyn ToSql + Sync))
            .collect::<Vec<_>>();
        Ok(self.runtime.block_on(self.client.execute(sql, &params))? as usize)
    }
}

#[derive(Debug)]
struct Param<'a>(&'a V);
impl ToSql for Param<'_> {
    fn to_sql(
        &self,
        typ: &Type,
        out: &mut BytesMut,
    ) -> std::result::Result<IsNull, Box<dyn Error + Sync + Send>> {
        let value = self.0;
        if value.is_none() {
            return Ok(IsNull::Yes);
        }
        match *typ {
            Type::INT8 => value.as_i64()?.to_sql(typ, out),
            Type::INT4 => i32::try_from(value.as_i64()?)?.to_sql(typ, out),
            Type::INT2 => i16::try_from(value.as_i64()?)?.to_sql(typ, out),
            Type::FLOAT8 => value.float().to_sql(typ, out),
            Type::FLOAT4 => (value.float() as f32).to_sql(typ, out),
            Type::BOOL => value.truth().to_sql(typ, out),
            Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::UNKNOWN => {
                value.text().to_sql(typ, out)
            }
            Type::BYTEA => value.raw()?.to_sql(typ, out),
            _ => Err(format!("unsupported PostgreSQL parameter type {typ}").into()),
        }
    }
    fn accepts(_: &Type) -> bool {
        true
    }
    to_sql_checked!();
}
struct Field(V);
impl<'a> FromSql<'a> for Field {
    fn from_sql(
        typ: &Type,
        raw: &'a [u8],
    ) -> std::result::Result<Self, Box<dyn Error + Sync + Send>> {
        Ok(Self(match *typ {
            Type::INT8 => i64::from_sql(typ, raw)?.into(),
            Type::INT4 => i32::from_sql(typ, raw)?.into(),
            Type::INT2 => (i16::from_sql(typ, raw)? as i64).into(),
            Type::FLOAT8 => f64::from_sql(typ, raw)?.into(),
            Type::FLOAT4 => (f32::from_sql(typ, raw)? as f64).into(),
            Type::BOOL => i32::from(bool::from_sql(typ, raw)?).into(),
            Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::UNKNOWN => {
                String::from_sql(typ, raw)?.into()
            }
            Type::BYTEA => V::Bytes(Vec::<u8>::from_sql(typ, raw)?),
            _ => return Err(format!("unsupported PostgreSQL result type {typ}").into()),
        }))
    }
    fn from_sql_null(_: &Type) -> std::result::Result<Self, Box<dyn Error + Sync + Send>> {
        Ok(Self(V::None))
    }
    fn accepts(_: &Type) -> bool {
        true
    }
}
