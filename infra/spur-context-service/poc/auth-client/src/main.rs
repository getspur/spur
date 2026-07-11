use spur_context_auth_client::{M2mClient, M2mConfig};

const HELP: &str = "\
spur-context-auth-client — standalone Cognito OAuth/OIDC POC\n\n\
Usage:\n\
  spur-context-auth-client m2m-token\n\n\
M2M configuration is read only from the environment:\n\
  SPUR_AUTH_CLIENT_ID\n\
  SPUR_AUTH_CLIENT_SECRET\n\
  SPUR_AUTH_TOKEN_ENDPOINT\n\
  SPUR_AUTH_SCOPES\n\n\
Human PKCE/OIDC library configuration is read only from:\n\
  SPUR_AUTH_ISSUER\n\
  SPUR_AUTH_AUTHORIZATION_ENDPOINT\n\
  SPUR_AUTH_TOKEN_ENDPOINT\n\
  SPUR_AUTH_HUMAN_CLIENT_ID\n\
  SPUR_AUTH_REDIRECT_URI\n\n\
No secret, token, authorization code, or PKCE verifier is accepted as an argument.\n\
The command never writes an acquired bearer token to stdout or stderr.\n";

#[tokio::main]
async fn main() {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let exit_code = match arguments.as_slice() {
        [] => {
            print!("{HELP}");
            0
        }
        [command] if matches!(command.as_str(), "--help" | "-h" | "help") => {
            print!("{HELP}");
            0
        }
        [command] if command == "m2m-token" => match acquire_m2m_token().await {
            Ok(()) => {
                println!("M2M access token acquired for this process.");
                0
            }
            Err(error) => {
                eprintln!("Authentication failed: {error}");
                1
            }
        },
        _ => {
            eprintln!("Unsupported command. Run with --help for safe configuration guidance.");
            2
        }
    };
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}

async fn acquire_m2m_token() -> Result<(), spur_context_auth_client::ClientError> {
    let config = M2mConfig::from_environment()?;
    let client = M2mClient::new(config)?;
    let _access_token = client.access_token().await?;
    Ok(())
}
