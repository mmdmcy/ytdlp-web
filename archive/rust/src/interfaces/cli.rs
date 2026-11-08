use std::{env, io};

pub(crate) async fn run() -> io::Result<()> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    match args.first().map(String::as_str).unwrap_or("serve") {
        "serve" => crate::app::serve().await,
        "hash-password" => {
            crate::security::auth::hash_password_cmd(&args[1..])?;
            Ok(())
        }
        "audit-public" => {
            if crate::security::public_audit::audit_public_cmd(&args[1..])? != 0 {
                std::process::exit(1);
            }
            Ok(())
        }
        _ => {
            eprintln!("usage: ytdlp-web [serve|hash-password --stdin|audit-public]");
            Ok(())
        }
    }
}
