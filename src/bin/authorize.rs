use grammers_client::{Client, Config, InitParams};
use grammers_session::Session;
use std::env;
use std::fs;
use std::io::{self, Write};

/// extracts phone number digits only, removing all formatting
fn sanitize_phone_number(phone: &str) -> String {
    phone.chars().filter(|c| c.is_ascii_digit()).collect()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // load .env file if it exists
    let _ = dotenvy::dotenv();

    let api_id = env::var("TG_API_ID")
        .map_err(|_| "TG_API_ID environment variable is required")?
        .parse::<i32>()
        .map_err(|_| "TG_API_ID must be a valid integer")?;

    let api_hash =
        env::var("TG_API_HASH").map_err(|_| "TG_API_HASH environment variable is required")?;

    println!("Connecting to Telegram...");

    print!("Enter your phone number (international format, e.g., +1234567890): ");
    io::stdout().flush()?;
    let mut phone = String::new();
    io::stdin().read_line(&mut phone)?;
    let phone = phone.trim();

    // sanitize phone number for filename
    let phone_digits = sanitize_phone_number(phone);

    // get current directory and create absolute paths
    let current_dir = env::current_dir()?;
    let sessions_dir = current_dir.join("sessions");
    let session_path = sessions_dir.join(format!("{}.session", phone_digits));

    // ensure sessions directory exists
    if !sessions_dir.exists() {
        println!("Creating sessions directory...");
        fs::create_dir_all(&sessions_dir)?;
    }

    // try to load existing session first
    let session = match Session::load_file(session_path.to_str().unwrap()) {
        Ok(session) => {
            println!("Loaded existing session from {}", session_path.display());
            session
        }
        Err(_) => {
            println!("Creating new session");
            Session::new()
        }
    };

    let config = Config {
        session,
        api_id,
        api_hash,
        params: InitParams {
            ..Default::default()
        },
    };

    let client = Client::connect(config).await?;

    if !client.is_authorized().await? {
        println!("You are not authorized. Let's do that now.");

        let token = client.request_login_code(phone).await?;

        print!("Enter the code you received: ");
        io::stdout().flush()?;
        let mut code = String::new();
        io::stdin().read_line(&mut code)?;
        let code = code.trim();

        match client.sign_in(&token, code).await {
            Ok(_) => println!("Authorization successful!"),
            Err(grammers_client::SignInError::PasswordRequired(password_token)) => {
                print!("Two-step verification enabled. Enter your password: ");
                io::stdout().flush()?;
                let mut password = String::new();
                io::stdin().read_line(&mut password)?;
                let password = password.trim();

                client.check_password(password_token, password).await?;
                println!("Authorization successful!");
            }
            Err(e) => return Err(e.into()),
        }
    } else {
        println!("Already authorized!");
    }

    // save the session
    match client
        .session()
        .save_to_file(session_path.to_str().unwrap())
    {
        Ok(_) => println!(
            "Session saved successfully to {} for phone number {}",
            session_path.display(),
            phone
        ),
        Err(e) => {
            eprintln!("Failed to save session: {}", e);
            eprintln!("This might be a permissions issue or the grammers library version issue.");
            eprintln!("Try running the program with sudo or check if the session file can be created manually.");
            return Err(e.into());
        }
    }

    Ok(())
}
