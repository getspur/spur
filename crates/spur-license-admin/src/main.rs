use clap::Parser as _;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();

    let cli = spur_license_admin::cli::Cli::parse();

    match cli.command {
        spur_license_admin::cli::Commands::SignPolicy {
            input,
            output,
            key_id,
            signing_key,
        } => {
            spur_license_admin::commands::sign_policy::run(
                &input,
                output.as_deref(),
                &key_id,
                &signing_key,
            )
            .await?;
        }
        spur_license_admin::cli::Commands::License { action } => match action {
            spur_license_admin::cli::LicenseAction::Create {
                plan,
                email,
                seats,
                secret_key,
                product,
            } => {
                let client = spur_license_admin::api::AdminClient::new(
                    secret_key.expose(),
                    &product,
                    "https://licenseseat.com/api/v1",
                );
                let value = client
                    .create_license(&plan, email.as_deref(), seats)
                    .await?;
                println!("{}", serde_json::to_string_pretty(&value)?);
            }
            spur_license_admin::cli::LicenseAction::Revoke {
                key,
                secret_key,
                product,
            } => {
                let client = spur_license_admin::api::AdminClient::new(
                    secret_key.expose(),
                    &product,
                    "https://licenseseat.com/api/v1",
                );
                let value = client.revoke_license(&key).await?;
                println!("{}", serde_json::to_string_pretty(&value)?);
            }
            spur_license_admin::cli::LicenseAction::Activations {
                key,
                secret_key,
                product,
            } => {
                let client = spur_license_admin::api::AdminClient::new(
                    secret_key.expose(),
                    &product,
                    "https://licenseseat.com/api/v1",
                );
                let value = client.list_activations(&key).await?;
                println!("{}", serde_json::to_string_pretty(&value)?);
            }
        },
    }

    Ok(())
}
