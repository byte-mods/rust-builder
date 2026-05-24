use serde::{Deserialize, Serialize};
use sqlx::{AnyConnection, Connection, Executor, Row};
use crate::error::ApiError;

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct DbColumn {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub primary_key: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct DbRelation {
    pub column: String,
    pub referenced_table: String,
    pub referenced_column: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct DbTable {
    pub name: String,
    pub columns: Vec<DbColumn>,
    pub relations: Vec<DbRelation>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct DbSchemaReport {
    pub tables: Vec<DbTable>,
}

/// Introspect structural tables, columns, and foreign key relations for PostgreSQL or SQLite databases.
pub async fn introspect_schema(connection_string: &str) -> Result<DbSchemaReport, ApiError> {
    sqlx::any::install_default_drivers();

    // 1. Establish the connection using sqlx::any (supporting postgres/sqlite dynamically)
    let mut conn = AnyConnection::connect(connection_string)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to connect to database: {e}")))?;

    let is_postgres = connection_string.starts_with("postgres:") || connection_string.starts_with("postgresql:");

    if is_postgres {
        introspect_postgres(&mut conn).await
    } else {
        introspect_sqlite(&mut conn).await
    }
}

async fn introspect_postgres(conn: &mut AnyConnection) -> Result<DbSchemaReport, ApiError> {
    // 1. Query all columns in the public schema
    let col_query = r#"
        SELECT 
            table_name,
            column_name,
            data_type,
            is_nullable
        FROM 
            information_schema.columns 
        WHERE 
            table_schema = 'public'
        ORDER BY 
            table_name, ordinal_position;
    "#;
    let col_rows = conn.fetch_all(col_query)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to query postgres columns: {e}")))?;

    // 2. Query all primary keys in public schema
    let pk_query = r#"
        SELECT 
            kcu.table_name,
            kcu.column_name
        FROM 
            information_schema.table_constraints tc
        JOIN 
            information_schema.key_column_usage kcu ON tc.constraint_name = kcu.constraint_name
        WHERE 
            tc.constraint_type = 'PRIMARY KEY' AND tc.table_schema = 'public';
    "#;
    let pk_rows = conn.fetch_all(pk_query)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to query postgres primary keys: {e}")))?;

    // Track primary keys as a set of (table_name, column_name)
    let mut pks = std::collections::HashSet::new();
    for row in pk_rows {
        let t_name: String = row.try_get("table_name").unwrap_or_default();
        let c_name: String = row.try_get("column_name").unwrap_or_default();
        pks.insert((t_name, c_name));
    }

    // 3. Query all foreign keys in public schema
    let fk_query = r#"
        SELECT
            tc.table_name AS from_table,
            kcu.column_name AS from_column,
            ccu.table_name AS to_table,
            ccu.column_name AS to_column
        FROM
            information_schema.table_constraints AS tc
            JOIN information_schema.key_column_usage AS kcu
              ON tc.constraint_name = kcu.constraint_name
            JOIN information_schema.constraint_column_usage AS ccu
              ON ccu.constraint_name = tc.constraint_name
        WHERE tc.constraint_type = 'FOREIGN KEY' AND tc.table_schema = 'public';
    "#;
    let fk_rows = conn.fetch_all(fk_query)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to query postgres foreign keys: {e}")))?;

    // Track relations mapped by table_name -> Vec<DbRelation>
    let mut relations_map: std::collections::HashMap<String, Vec<DbRelation>> = std::collections::HashMap::new();
    for row in fk_rows {
        let from_t: String = row.try_get("from_table").unwrap_or_default();
        let from_c: String = row.try_get("from_column").unwrap_or_default();
        let to_t: String = row.try_get("to_table").unwrap_or_default();
        let to_c: String = row.try_get("to_column").unwrap_or_default();

        relations_map.entry(from_t).or_default().push(DbRelation {
            column: from_c,
            referenced_table: to_t,
            referenced_column: to_c,
        });
    }

    // 4. Assemble tables list
    let mut tables_map: std::collections::HashMap<String, Vec<DbColumn>> = std::collections::HashMap::new();
    let mut ordered_tables = Vec::new();

    for row in col_rows {
        let t_name: String = row.try_get("table_name").unwrap_or_default();
        let c_name: String = row.try_get("column_name").unwrap_or_default();
        let d_type: String = row.try_get("data_type").unwrap_or_default();
        let is_null: String = row.try_get("is_nullable").unwrap_or_default();

        let nullable = is_null == "YES";
        let primary_key = pks.contains(&(t_name.clone(), c_name.clone()));

        let col = DbColumn {
            name: c_name,
            data_type: d_type,
            nullable,
            primary_key,
        };

        if !tables_map.contains_key(&t_name) {
            ordered_tables.push(t_name.clone());
        }
        tables_map.entry(t_name).or_default().push(col);
    }

    let mut tables = Vec::new();
    for t_name in ordered_tables {
        let columns = tables_map.remove(&t_name).unwrap_or_default();
        let relations = relations_map.remove(&t_name).unwrap_or_default();
        tables.push(DbTable {
            name: t_name,
            columns,
            relations,
        });
    }

    Ok(DbSchemaReport { tables })
}

async fn introspect_sqlite(conn: &mut AnyConnection) -> Result<DbSchemaReport, ApiError> {
    // 1. Get all user tables
    let tables_query = "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name;";
    let table_rows = conn.fetch_all(tables_query)
        .await
        .map_err(|e| ApiError::Internal(format!("failed to query sqlite tables: {e}")))?;

    let mut tables = Vec::new();

    for t_row in table_rows {
        let t_name: String = t_row.try_get("name").unwrap_or_default();
        
        // Sanitize table name to protect against SQL injection inside PRAGMAs
        if !t_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }

        // 2. Query columns using PRAGMA table_info
        let info_query = format!("PRAGMA table_info({});", t_name);
        let info_rows = conn.fetch_all(&*info_query)
            .await
            .map_err(|e| ApiError::Internal(format!("failed to query sqlite table_info for '{t_name}': {e}")))?;

        let mut columns = Vec::new();
        for col_row in info_rows {
            let name: String = col_row.try_get("name").unwrap_or_default();
            let d_type: String = col_row.try_get("type").unwrap_or_default();
            let not_null: i32 = col_row.try_get("notnull").unwrap_or(0);
            let pk: i32 = col_row.try_get("pk").unwrap_or(0);

            columns.push(DbColumn {
                name,
                data_type: d_type.to_lowercase(),
                nullable: not_null == 0,
                primary_key: pk > 0,
            });
        }

        // 3. Query foreign keys using PRAGMA foreign_key_list
        let fk_query = format!("PRAGMA foreign_key_list({});", t_name);
        let fk_rows = conn.fetch_all(&*fk_query)
            .await
            .map_err(|e| ApiError::Internal(format!("failed to query sqlite foreign_key_list for '{t_name}': {e}")))?;

        let mut relations = Vec::new();
        for fk_row in fk_rows {
            let referenced_table: String = fk_row.try_get("table").unwrap_or_default();
            let column: String = fk_row.try_get("from").unwrap_or_default();
            let referenced_column: String = fk_row.try_get("to").unwrap_or_default();

            relations.push(DbRelation {
                column,
                referenced_table,
                referenced_column,
            });
        }

        tables.push(DbTable {
            name: t_name,
            columns,
            relations,
        });
    }

    Ok(DbSchemaReport { tables })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_sqlite_schema_introspection() {
        sqlx::any::install_default_drivers();
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test_introspect.sqlite");
        std::fs::File::create(&db_path).unwrap();
        let conn_str = format!("sqlite://{}", db_path.to_str().unwrap());

        // Create test tables and key constraints
        {
            let mut conn = AnyConnection::connect(&conn_str).await.unwrap();
            conn.execute("CREATE TABLE profiles (id INTEGER PRIMARY KEY NOT NULL, name TEXT NOT NULL);").await.unwrap();
            conn.execute(
                "CREATE TABLE users (
                    id INTEGER PRIMARY KEY NOT NULL,
                    email TEXT NOT NULL UNIQUE,
                    profile_id INTEGER,
                    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                    FOREIGN KEY(profile_id) REFERENCES profiles(id)
                );"
            ).await.unwrap();
        }

        // Run introspection
        let report = introspect_schema(&conn_str).await.unwrap();
        assert_eq!(report.tables.len(), 2);

        // Assert profiles table
        let profiles = report.tables.iter().find(|t| t.name == "profiles").unwrap();
        assert_eq!(profiles.columns.len(), 2);
        assert_eq!(profiles.columns[0].name, "id");
        assert_eq!(profiles.columns[0].data_type, "integer");
        assert!(profiles.columns[0].primary_key);
        assert!(!profiles.columns[0].nullable);

        assert_eq!(profiles.columns[1].name, "name");
        assert_eq!(profiles.columns[1].data_type, "text");
        assert!(!profiles.columns[1].primary_key);
        assert!(!profiles.columns[1].nullable);

        // Assert users table
        let users = report.tables.iter().find(|t| t.name == "users").unwrap();
        assert_eq!(users.columns.len(), 4);
        let id_col = users.columns.iter().find(|c| c.name == "id").unwrap();
        assert!(id_col.primary_key);

        let email_col = users.columns.iter().find(|c| c.name == "email").unwrap();
        assert!(!email_col.primary_key);
        assert!(!email_col.nullable);

        let profile_col = users.columns.iter().find(|c| c.name == "profile_id").unwrap();
        assert!(profile_col.nullable);

        // Assert foreign key relation
        assert_eq!(users.relations.len(), 1);
        assert_eq!(users.relations[0].column, "profile_id");
        assert_eq!(users.relations[0].referenced_table, "profiles");
        assert_eq!(users.relations[0].referenced_column, "id");
    }
}
