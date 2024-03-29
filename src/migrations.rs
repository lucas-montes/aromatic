use std::{
    collections::HashMap,
    fs::{read_dir, DirEntry},
    path::PathBuf,
};

use menva::{get_bool_env, get_env};
use sqlx::{
    migrate::MigrateDatabase, sqlite::SqliteConnection, FromRow, Sqlite, SqlitePool,
    Transaction,
};
use tracing::error;

use super::Orm;

#[derive(Debug)]
enum MigrationError {
    Failed,
}

#[allow(dead_code)]
#[derive(FromRow, Debug)]
struct Migration {
    id: u32,
    name: String,
    path: String,
    ran: bool,
    timestamp: String,
}

#[derive(Debug)]
struct MigrationFile {
    name: String,
    ran: bool,
    path: PathBuf,
}

impl MigrationFile {
    fn new(entry: DirEntry) -> Self {
        Self {
            name: entry.file_name().to_string_lossy().to_string(),
            ran: false,
            path: entry.path(),
        }
    }
}

pub async fn migrate(folder_path: &str) {
    let db_url = get_env("DATABASE_URL");
    create_database(&db_url).await;

    let mut transaction = match transaction().await {
        Ok(t) => t,
        Err(err) => {
            error!(
                function = "migrate",
                error_message = format!("{err}"),
                message = "Could not start transaction",
            );
            return;
        },
    };
    let _ = create_migrations_table(&mut transaction)
        .await
        .map_err(|err| {
            error!(
                function = "create_migrations_table",
                error_message = format!("{err}"),
                message = "Could not create the migrations table",
            );
        });
    let migrations_history = match get_migrations_history(&mut transaction).await {
        Ok(m) => m,
        Err(err) => {
            error!(
                function = "get_migrations_history",
                error_message = format!("{err}"),
                message = "Could not get migrations history",
            );
            return;
        },
    };

    let migrations_files = match get_migrations_files(folder_path).await {
        Ok(m) => m,
        Err(err) => {
            error!(
                function = "get_migrations_files",
                error_message = format!("{err}"),
                message = "Could not get migrations files",
            );
            return;
        },
    };
    // maybe just loop over all the files migrations, save them into the database if they don0t exists.
    // then query the database to get the list of migrations and execute them.
    match migrations_history.is_empty() {
        true => run_inital_migrations(migrations_files, &mut transaction).await,
        false => {
            run_migrations(migrations_files, migrations_history, &mut transaction).await
        },
    }
    match commit_transaction(transaction).await {
        Ok(_) => (),
        Err(err) => error!(
            function = "commit_transaction",
            error_message = format!("{err}"),
            message = "Could not commit migrations",
        ),
    };
}

async fn create_database(db_url: &str) {
    match Sqlite::create_database(db_url).await {
        Ok(_) => (),
        Err(err) => {
            error!(
                function = "create_database",
                error_message = format!("{err}"),
                message = "Error creating the database",
            );
        },
    };
}

async fn create_migrations_table<'a>(
    transaction: &mut Transaction<'a, Sqlite>,
) -> Result<u64, sqlx::Error> {
    let query = r#"
        CREATE TABLE IF NOT EXISTS migrations (
            id INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            path TEXT NOT NULL,
            ran BOOLEAN NOT NULL,
            timestamp TIMESTAMP DEFAULT CURRENT_TIMESTAMP
        );
    "#;
    let result = sqlx::query(query)
        .execute(transaction as &mut SqliteConnection)
        .await?;
    Ok(result.rows_affected())
}

async fn get_migrations_history<'a>(
    transaction: &mut Transaction<'a, Sqlite>,
) -> Result<Vec<Migration>, sqlx::Error> {
    let query = Orm::select("*").from("migrations").ready();
    let rows = sqlx::query_as::<_, Migration>(&query)
        .fetch_all(transaction as &mut SqliteConnection)
        .await;

    match rows {
        Ok(result) => Ok(result),
        Err(err) => {
            error!(
                function = "get_migrations_history",
                error_message = format!("{err}"),
                message = "Error finding the migration",
            );
            Err(err)
        },
    }
}

async fn get_migrations_files(
    folder_path: &str,
) -> Result<Vec<MigrationFile>, std::io::Error> {
    let entries = match read_dir(folder_path) {
        Ok(result) => result,
        Err(err) => {
            error!(
                function = "get_migrations_history",
                error_message = format!("{err}"),
                message = "error reading dir",
            );
            return Err(err);
        },
    };

    Ok(entries
        .into_iter()
        .map(|f| MigrationFile::new(f.ok().unwrap()))
        .collect())
}

async fn run_migrations<'a>(
    migrations_files: Vec<MigrationFile>,
    migrations_history: Vec<Migration>,
    transaction: &mut Transaction<'a, Sqlite>,
) {
    let mut migrations_to_save = HashMap::new();
    migrations_history.iter().for_each(|m| {
        migrations_to_save.insert(&m.name, m);
    });

    for mut migration_file in migrations_files {
        let mut id_to_update = None;
        if let Some(migration) = migrations_to_save.get(&migration_file.name) {
            if skip_migration(
                migration.ran,
                &migration.name,
                get_bool_env("RUN_TEST_MIGRATIONS"),
            )
            .await
            {
                continue;
            } else {
                id_to_update = Some(migration.id);
            }
        };
        make_migration(&mut migration_file, transaction, id_to_update).await;
    }
}

async fn run_inital_migrations<'a>(
    migrations_files: Vec<MigrationFile>,
    transaction: &mut Transaction<'a, Sqlite>,
) {
    for mut migration_file in migrations_files {
        if skip_migration(
            migration_file.ran,
            &migration_file.name,
            get_bool_env("RUN_TEST_MIGRATIONS"),
        )
        .await
        {
            continue;
        }
        make_migration(&mut migration_file, transaction, None).await;
    }
}

async fn skip_migration(
    migration_has_been_run: bool,
    name: &str,
    run_test_migrations: bool,
) -> bool {
    if migration_has_been_run {
        // if the migration has been ran we skip it
        true
    } else if run_test_migrations {
        // if the migration hasn't been ran and we want to run the tests migrations, we don't want to skip this migration
        false
    } else {
        // if we don't want to run the tests migrations we'll check if it contains "test" in the name
        // if it contains "test" we skip the migration
        name.contains("test")
    }
}

async fn make_migration<'a>(
    migration_file: &mut MigrationFile,
    transaction: &mut Transaction<'a, Sqlite>,
    id_to_update: Option<u32>,
) {
    match execute_migration(&migration_file.path, transaction).await {
        Ok(_) => {
            migration_file.ran = true;
            save_or_update(migration_file, transaction, id_to_update).await;
        },
        Err(err) => {
            error!(
                function = "make_migration",
                error_message = format!("{:?}", err),
                message = format!("Could not run migration {:?}", migration_file),
            );
        },
    }
}

async fn save_or_update<'a>(
    migration_file: &mut MigrationFile,
    transaction: &mut Transaction<'a, Sqlite>,
    id_to_update: Option<u32>,
) {
    let result = match id_to_update {
        Some(id) => update_migration_to_history(transaction, id).await,
        None => save_migration_to_history(migration_file, transaction).await,
    };
    match result {
        Ok(_) => (),
        Err(err) => {
            error!(
                function = "save_or_update",
                error_message = format!("{err}"),
                message = format!("Could not save migration {:?}", migration_file),
            );
        },
    }
}

async fn execute_migration<'a>(
    file_path: &PathBuf,
    transaction: &mut Transaction<'a, Sqlite>,
) -> Result<u64, MigrationError> {
    let query = match tokio::fs::read_to_string(file_path).await {
        Ok(sql) => sql,
        Err(err) => {
            error!(
                function = "execute_migration",
                error_message = format!("{err}"),
                message = "error reading files",
            );
            return Err(MigrationError::Failed);
        },
    };
    match sqlx::query(&query)
        .execute(transaction as &mut SqliteConnection)
        .await
    {
        Ok(row) => Ok(row.rows_affected()),
        Err(err) => {
            error!(
                function = "execute_migration",
                error_message = format!("{err}"),
                message = "Error executing th emigration",
            );
            Err(MigrationError::Failed)
        },
    }
}

async fn update_migration_to_history<'a>(
    transaction: &mut Transaction<'a, Sqlite>,
    id_to_update: u32,
) -> Result<u64, sqlx::Error> {
    let query = Orm::update("migrations")
        .set("ran = true")
        .where_()
        .equal("id", &format!("{}", id_to_update))
        .ready();
    match sqlx::query(&query)
        .execute(transaction as &mut SqliteConnection)
        .await
    {
        Ok(row) => Ok(row.rows_affected()),
        Err(err) => {
            error!(
                function = "update_migration_to_history",
                error_message = format!("{err}"),
                message = "Error during updating migration into the history",
            );
            Err(err)
        },
    }
}

async fn save_migration_to_history<'a>(
    migration_file: &MigrationFile,
    transaction: &mut Transaction<'a, Sqlite>,
) -> Result<u64, sqlx::Error> {
    let query = Orm::insert("migrations")
        .set_columns("name,path,ran")
        .add_value(&format!(
            "'{}','{}',{}",
            migration_file.name,
            migration_file.path.display(),
            migration_file.ran
        ))
        .ready();
    match sqlx::query(&query)
        .execute(transaction as &mut SqliteConnection)
        .await
    {
        Ok(row) => Ok(row.rows_affected()),
        Err(err) => {
            error!(
                function = "save_migration_to_history",
                error_message = format!("{err}"),
                message = "Error during saving the new migration into the history",
            );
            Err(err)
        },
    }
}

async fn commit_transaction(
    transaction: Transaction<'_, Sqlite>,
) -> Result<(), sqlx::Error> {
    match transaction.commit().await {
        Ok(_) => Ok(()),
        Err(err) => {
            error!(
                function = "commit_transaction",
                error_message = format!("{err}"),
                message = "transaction commit error",
            );
            Err(err)
        },
    }
}

async fn transaction<'a>() -> Result<Transaction<'a, Sqlite>, sqlx::Error> {
    match connect().await.begin().await {
        Ok(transaction) => Ok(transaction),
        Err(err) => {
            error!(
                function = "transaction",
                error_message = format!("{err}"),
                message = "transaction errror launching",
            );
            Err(err)
        },
    }
}

async fn connect() -> SqlitePool {
    match SqlitePool::connect(&get_env("DATABASE_URL")).await {
        Ok(db) => db,
        Err(err) => {
            error!(
                function = "connect",
                error_message = format!("{err}"),
                message = "Error connecting to database",
            );
            panic!()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_skip_migration_skip_test_migrations() {
        assert!(!skip_migration(false, "migration", false).await);

        assert!(skip_migration(true, "migration", false).await);

        assert!(skip_migration(false, "test_migration", false).await);

        assert!(skip_migration(true, "test_migration", false).await);
    }

    #[tokio::test]
    async fn test_skip_migration_run_test_migrations() {
        assert!(!skip_migration(false, "migration", true).await);

        assert!(skip_migration(true, "migration", true).await);

        assert!(!skip_migration(false, "test_migration", true).await);

        assert!(skip_migration(true, "test_migration", true).await);
    }
}
