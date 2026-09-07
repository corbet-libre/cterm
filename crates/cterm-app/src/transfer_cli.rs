//! Windowless `cterm transfer` management command.
//!
//! Talks to the running local daemon over its normal client connection and
//! exits: minting, listing, or revoking decided-policy same-TTY transfer
//! pre-authorizations. Managed mode is out of scope (it disables the whole
//! transfer surface); this always dials the default local daemon.

use anyhow::{Context, Result};

use crate::cli::TransferCommand;

/// If `args` carries a windowless subcommand, run it and return its process
/// exit code for the backend to exit with. An explicit `--execute` child
/// always wins over a subcommand, so launching a child whose first argument
/// is literally `transfer` keeps its long-standing meaning.
pub fn run_if_requested(args: &crate::cli::Args) -> Option<i32> {
    if args.command.is_some() {
        return None;
    }
    let command = match args.subcommand.as_ref() {
        Some(crate::cli::UiCommand::Transfer(command)) => command,
        None => return None,
    };
    match run(command) {
        Ok(()) => Some(0),
        Err(error) => {
            eprintln!("cterm: {error:#}");
            Some(1)
        }
    }
}

/// Run `command` against the local daemon, printing one human-readable result.
/// Exactly one of `--allow-once` / `--list` / `--revoke` is required.
pub fn run(command: &TransferCommand) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to start transfer-command runtime")?;
    runtime.block_on(run_async(command))
}

async fn run_async(command: &TransferCommand) -> Result<()> {
    let session = command.session.as_deref().context(
        "a daemon session id is required (`--session <id>`); run this inside the session it should cover",
    )?;
    let connection = cterm_client::DaemonConnection::connect_local()
        .await
        .context("failed to connect to the local cterm daemon")?;

    if command.allow_once {
        let ok = connection
            .pre_authorize_transfer(session, command.transfer_id.clone())
            .await?;
        if !ok {
            anyhow::bail!("daemon refused the pre-authorization for session {session}");
        }
        match &command.transfer_id {
            Some(transfer_id) => println!(
                "Pre-authorized transfer {transfer_id} in session {session}: \
                 its next session-start skips one consent dialog."
            ),
            None => println!(
                "Pre-authorized session {session}: \
                 its next transfer skips one consent dialog."
            ),
        }
    } else if command.list {
        let entries = connection.list_transfer_pre_authorizations(session).await?;
        if entries.is_empty() {
            println!("No live pre-authorizations for session {session}.");
        }
        for entry in entries {
            match entry {
                Some(transfer_id) => {
                    println!("transfer {transfer_id} (single-use, dies with session)")
                }
                None => println!("next transfer in session (single-use, dies with session)"),
            }
        }
    } else if command.revoke {
        let revoked = connection
            .revoke_transfer_pre_authorization(session, command.transfer_id.clone())
            .await?;
        match (&command.transfer_id, revoked) {
            (Some(transfer_id), true) => {
                println!(
                    "Revoked pre-authorization for transfer {transfer_id} in session {session}."
                )
            }
            (Some(transfer_id), false) => {
                println!("Nothing armed for transfer {transfer_id} in session {session}.")
            }
            (None, true) => {
                println!("Revoked the armed next-transfer pre-authorization in session {session}.")
            }
            (None, false) => {
                println!("No next-transfer pre-authorization armed in session {session}.")
            }
        }
    } else {
        anyhow::bail!("exactly one of --allow-once, --list, or --revoke is required");
    }
    Ok(())
}
