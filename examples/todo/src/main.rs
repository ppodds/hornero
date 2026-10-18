use hornero::BoxError;
use todo::app::ServerConfig;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,todo=debug")),
        )
        .init();

    // Bootstrap first: the listen address is itself configuration.
    let app = todo::hornero().build().await?;
    let server = app
        .get::<ServerConfig>()
        .expect("ServerConfig is provided by the global ConfigModule");
    let listener = match TcpListener::bind(&server.address).await {
        Ok(listener) => listener,
        Err(err) => {
            // Dropping the app would skip its shutdown hooks (the database connection is never
            // closed), so run them before reporting the bind error.
            if let Err(close_err) = app.close().await {
                tracing::error!(error = %BoxError::from(close_err), "shutdown after failed bind also failed");
            }
            return Err(err.into());
        }
    };
    tracing::info!(address = %listener.local_addr()?, "todo API is listening");
    app.serve(listener).await
}
