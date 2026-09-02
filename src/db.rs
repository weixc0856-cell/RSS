use worker::{D1Database, Env, Result};

/// Get D1 database from environment.
pub fn get_db(env: &Env) -> Result<D1Database> {
    env.d1("rss_db")
}

