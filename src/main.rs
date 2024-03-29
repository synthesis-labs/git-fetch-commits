use byte_unit::{Byte, UnitType};
use clap::Parser;
use git2::{Cred, Diff, RemoteCallbacks, Sort};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use serde::Serialize;
use std::{cell::Cell, str::FromStr};
use tempfile::tempdir;

#[derive(Serialize, Clone, Debug)]
struct FileChange {
    path: String,
    lines_added: u32,
    lines_removed: u32,
    lines_modified: u32,
    hunks_added: u32,
    hunks_removed: u32,
    hunks_modified: u32,
}

#[derive(Serialize, Debug, Clone)]
enum CommitType {
    Normal,
    Merge,
}

#[derive(Serialize, Debug)]
struct Commit {
    id: String,
    repo_url: String,
    timestamp: i64,
    author_name: String,
    author_email: String,
    message: String,
    r#type: CommitType,
    changes: Vec<FileChange>,
}

#[derive(Serialize, Debug)]
struct FlatCommit {
    id: String,
    repo_url: String,
    timestamp: i64,
    author_name: String,
    author_email: String,
    message: String,
    r#type: CommitType,
    path: String,
    lines_added: u32,
    lines_removed: u32,
    lines_modified: u32,
    hunks_added: u32,
    hunks_removed: u32,
    hunks_modified: u32,
}

fn extract_from_diff(diff: &Diff) -> Result<Vec<FileChange>, git2::Error> {
    // diff.foreach works in a very imperative way, looping through the diffs
    // and calling callbacks in serial until it's complete
    //
    let mut files: Vec<FileChange> = Vec::new();

    // Using a cell so we can modify the captured FileChange via the multiple closures below
    // without ownership issues
    //
    let x: Cell<Option<FileChange>> = Cell::new(None);

    diff.foreach(
        &mut |diff_delta, _s| {
            match x.take() {
                // If we're currently busy with a file, this means we're moving on so we
                // should push it and replace it with the new one
                //
                Some(file_change) => {
                    files.push(file_change);
                }
                _ => {}
            }
            let filename = diff_delta.new_file().path().unwrap().to_str().unwrap();

            x.set(Some(FileChange {
                path: String::from_str(filename).unwrap(),
                lines_added: 0,
                lines_removed: 0,
                lines_modified: 0,
                hunks_added: 0,
                hunks_removed: 0,
                hunks_modified: 0,
            }));
            true
        },
        None,
        Some(&mut |_diff_delta, diff_hunk| {
            // Guaranteed to be processing a file (big assumption?)
            //
            let state = x.take().unwrap();
            let updated = match (diff_hunk.old_lines(), diff_hunk.new_lines()) {
                (0, _) => FileChange {
                    hunks_added: state.hunks_added + 1,
                    ..state
                },
                (_, 0) => FileChange {
                    hunks_removed: state.hunks_removed + 1,
                    ..state
                },
                (_, _) => FileChange {
                    hunks_modified: state.hunks_modified + 1,
                    ..state
                },
            };
            x.set(Some(updated));
            true
        }),
        Some(&mut |_diff_delta, _diff_hunk, diff_line| {
            // Guaranteed to be processing a file (big assumption?)
            //
            let state = x.take().unwrap();
            let updated = match (diff_line.old_lineno(), diff_line.new_lineno()) {
                (None, Some(_)) => FileChange {
                    lines_added: state.lines_added + 1,
                    ..state
                },
                (Some(_), None) => FileChange {
                    lines_removed: state.lines_removed + 1,
                    ..state
                },
                (Some(_), Some(_)) => FileChange {
                    lines_modified: state.lines_modified + 1,
                    ..state
                },
                // Both being None is weird... don't think possible?
                _ => state,
            };
            x.set(Some(updated));
            true
        }),
    )?;

    Ok(files)
}

fn extract_logs(args: &Args) -> Result<(), git2::Error> {
    let multiprogress = MultiProgress::new();

    let sty = ProgressStyle::with_template(
        "[{elapsed_precise}] {bar:40.cyan/blue} {pos:>7}/{len:7} {msg}",
    )
    .unwrap()
    .progress_chars("#>-");

    let progress_client = multiprogress.add(ProgressBar::new(1));
    let progress_server = multiprogress.add(ProgressBar::new(1));
    let progress_transfer = multiprogress.add(ProgressBar::new(1));
    let progress_indexing = multiprogress.add(ProgressBar::new(1));
    progress_client.set_style(sty.clone());
    progress_client.set_message("Client progress");
    progress_server.set_style(sty.clone());
    progress_server.set_message("Server progress");
    progress_transfer.set_style(sty.clone());
    progress_transfer.set_message("Transfer progress");
    progress_indexing.set_style(sty.clone());
    progress_indexing.set_message("Indexing progress");

    let mut callbacks = RemoteCallbacks::new();

    callbacks.credentials(|_url, username_from_url, allowed_types| {
        // eprintln!(
        //     "Credentials callback for url={} username={} allowed={:?}",
        //     url,
        //     username_from_url.unwrap_or("none"),
        //     allowed_types
        // );

        progress_client.set_message("Authenticating...");

        if allowed_types.is_ssh_key() {
            // Provide ssh key from current agent
            //
            Cred::ssh_key_from_agent(username_from_url.unwrap_or("none"))
        } else if allowed_types.is_user_pass_plaintext() {
            // Provide plaintext username / password if provided in args
            //
            match (
                args.plaintext_username.as_ref(),
                args.plaintext_password.as_ref(),
            ) {
                (Some(username), Some(password)) => Cred::userpass_plaintext(username, password),
                _ => {
                    eprintln!("Asked for plaintext credentials but none provided - maybe add -U and -P options?");
                    Cred::default()
                }
            }
        } else {
            eprintln!("Unsupported credential type - returning default (which probably will fail)");
            Cred::default()
        }
    });

    callbacks.transfer_progress(|progress| {
        let received_byte = Byte::from_u128(u128::try_from(progress.received_bytes()).unwrap())
            .unwrap()
            .get_appropriate_unit(UnitType::Binary);

        // eprintln!(
        //     "Progress => Received {} of {}, indexed {}, bytes {}",
        //     progress.received_objects(),
        //     progress.total_objects(),
        //     progress.indexed_objects(),
        //     adjusted_byte.get_appropriate_unit(UnitType::Binary)
        // );

        progress_transfer.set_length(progress.total_objects() as u64);
        progress_transfer.set_position(progress.received_objects() as u64);
        progress_transfer.set_message(format!("Tranferred {received_byte:.2}"));

        progress_indexing.set_length(progress.total_objects() as u64);
        progress_indexing.set_position(progress.indexed_objects() as u64);

        true
    });

    callbacks.pack_progress(|pack_builder_stage, current, total| {
        eprintln!(
            "Packing => Stage {:?}, b {}, c {}",
            pack_builder_stage, current, total
        );
        ()
    });

    callbacks.sideband_progress(|sb| {
        let msg = format!(
            "Server: {}",
            String::from_utf8_lossy(sb)
                .to_string()
                .replace("\n", "")
                .replace("\r", "")
        );
        progress_server.set_message(msg);
        true
    });

    let mut fo = git2::FetchOptions::new();
    fo.remote_callbacks(callbacks);

    let mut builder = git2::build::RepoBuilder::new();
    builder.fetch_options(fo);

    let temp_dir = tempdir().map_err(|_e| git2::Error::from_str("TempDir failed!"))?;
    // eprintln!("Using tempdir => {}", temp_dir.path().to_str().unwrap());
    progress_client.set_message("Cloning...");
    let repo = builder.clone(args.repo_url.as_str(), &temp_dir.path())?;
    progress_client.set_message("Clone complete");

    // Create the revwalk
    //
    let mut revwalk = repo.revwalk()?;
    revwalk.set_sorting(Sort::TIME)?;
    // eprintln!("Adding head");
    revwalk.push_head()?;

    // Complete all progress bars
    //
    progress_client.finish();
    progress_server.finish();
    progress_transfer.finish();
    progress_indexing.finish();

    // Add all branches to the revwalk
    //
    let branches = repo.branches(None)?;
    for branch_r in branches {
        if let Ok((branch, _branch_type)) = branch_r {
            if !branch.is_head() {
                // eprintln!(
                //     "Adding branch => {} {:?}",
                //     branch.name()?.unwrap_or("unnamed"),
                //     branch_type
                // );
                if let Some(target) = branch.get().target() {
                    revwalk.push(target)?;
                } else {
                    // eprintln!("No valid oid...");
                }
            }
        }
    }

    while let Some(Ok(oid)) = revwalk.next() {
        let commit = repo.find_commit(oid)?;
        let commit_tree = repo.find_tree(commit.tree_id()).unwrap();

        // println!(
        //     "Oid => {}, Author => {} {}, Message => {}",
        //     oid,
        //     author.name().unwrap_or("unknown"),
        //     author.email().unwrap_or("unknown"),
        //     commit.message().unwrap_or("unknown")
        // );

        // ignore any commits which have more than 1 parent (i.e. a merge)
        //
        let parent_commit = if commit.parent_count() == 0 {
            // Its the origin commit (the seed of the tree)
            //
            None
        } else {
            Some(commit.parent(0).unwrap().tree_id())
        };

        let parent_tree = parent_commit.map(|oid| repo.find_tree(oid).unwrap());

        let default_commit = Commit {
            id: oid.to_string(),
            r#type: CommitType::Normal,
            repo_url: args.repo_url.to_string(),
            timestamp: commit.time().seconds(),
            author_name: commit.author().name().unwrap_or("unknown").to_string(),
            author_email: commit.author().email().unwrap_or("unknown").to_string(),
            message: commit.message().unwrap_or("unknown").to_string(),
            changes: Vec::new(),
        };

        // If it's a merge type (multiple parents), then we ignore the file changes
        //
        let my_commit = if commit.parent_count() > 1 {
            Commit {
                r#type: CommitType::Merge,
                ..default_commit
            }
        }
        // If it's a normal single (or zero) parent commit, then we process the diff
        // to the parent to grab file changes
        //
        else {
            let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&commit_tree), None)?;
            let file_changes = extract_from_diff(&diff)?;
            Commit {
                r#type: CommitType::Normal,
                changes: file_changes,
                ..default_commit
            }
        };

        let flat: Vec<FlatCommit> = my_commit
            .changes
            .iter()
            .map(|change| FlatCommit {
                id: my_commit.id.clone(),
                r#type: my_commit.r#type.clone(),
                repo_url: my_commit.repo_url.clone(),
                timestamp: my_commit.timestamp.clone(),
                author_name: my_commit.author_name.clone(),
                author_email: my_commit.author_email.clone(),
                message: my_commit.message.clone(),
                path: change.path.clone(),
                lines_added: change.lines_added.clone(),
                lines_removed: change.lines_removed.clone(),
                lines_modified: change.lines_modified.clone(),
                hunks_added: change.hunks_added.clone(),
                hunks_removed: change.hunks_removed.clone(),
                hunks_modified: change.hunks_modified.clone(),
            })
            .collect();

        flat.iter().for_each(|f| {
            let my_flat_json =
                serde_json::to_string(&f).map_err(|_e| git2::Error::from_str("Serde failed!"));

            if let Ok(f) = my_flat_json {
                println!("{}", f);
            }
        });
    }

    Ok(())
}

#[derive(Parser, Debug)]
struct Args {
    #[arg(short = 'U', help = "Username to provide for PLAINTEXT auth")]
    plaintext_username: Option<String>,

    #[arg(short = 'P', help = "Password to provide for PLAINTEXT auth")]
    plaintext_password: Option<String>,

    #[arg(help = "The URL of the repository to clone")]
    repo_url: String,
}

fn main() {
    let args = Args::parse();
    // eprintln!("Args => {:?}", args);

    match extract_logs(&args) {
        Ok(()) => {
            //eprintln!("Complete"),
        }
        Err(e) => eprintln!("Err {:?}", e),
    };

    ()
}
