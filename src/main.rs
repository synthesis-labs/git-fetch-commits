use byte_unit::Byte;
use git2::{Cred, Diff, RemoteCallbacks, Sort};
use serde::Serialize;
use std::{cell::Cell, env, str::FromStr};
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

#[derive(Serialize, Debug)]
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

fn extract_logs(repo_url: &str) -> Result<(), git2::Error> {
    let mut callbacks = RemoteCallbacks::new();

    callbacks.credentials(|url, username_from_url, allowed_types| {
        eprintln!(
            "Credentials callback for url={} username={} allowed={:?}",
            url,
            username_from_url.unwrap_or("none"),
            allowed_types
        );

        // For now just provide agent ssh creds
        //
        Cred::ssh_key_from_agent(username_from_url.unwrap_or("none"))
    });

    callbacks.transfer_progress(|progress| {
        let adjusted_byte = Byte::from_bytes(u128::try_from(progress.received_bytes()).unwrap());
        eprintln!(
            "Progress => Received {} of {}, indexed {}, bytes {}",
            progress.received_objects(),
            progress.total_objects(),
            progress.indexed_objects(),
            adjusted_byte.get_appropriate_unit(false)
        );
        true
    });

    callbacks.pack_progress(|pack_builder_stage, b, c| {
        eprintln!(
            "Packing => Stage {:?}, b {}, c {}",
            pack_builder_stage, b, c
        );
        ()
    });

    // I'm not sure what this is - but log it anyways
    //
    callbacks.sideband_progress(|sb| {
        eprintln!("Sideband => {}", sb.len());
        true
    });

    let mut fo = git2::FetchOptions::new();
    fo.remote_callbacks(callbacks);

    let mut builder = git2::build::RepoBuilder::new();
    builder.fetch_options(fo);

    let temp_dir = tempdir().map_err(|_e| git2::Error::from_str("TempDir failed!"))?;
    eprintln!("Using tempdir => {}", temp_dir.path().to_str().unwrap());
    let repo = builder.clone(repo_url, &temp_dir.path())?;

    // Create the revwalk
    //
    let mut revwalk = repo.revwalk()?;
    revwalk.set_sorting(Sort::TIME)?;
    eprintln!("Adding head");
    revwalk.push_head()?;

    // Add all branches to the revwalk
    //
    let branches = repo.branches(None)?;
    for branch_r in branches {
        if let Ok((branch, branch_type)) = branch_r {
            if !branch.is_head() {
                eprintln!(
                    "Adding branch => {} {:?}",
                    branch.name()?.unwrap_or("unnamed"),
                    branch_type
                );
                if let Some(target) = branch.get().target() {
                    revwalk.push(target)?;
                } else {
                    eprintln!("No valid oid...");
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
            repo_url: repo_url.to_string(),
            timestamp: commit.time().seconds(),
            author_name: commit.author().name().unwrap_or("unknown").to_string(),
            author_email: commit.author().email().unwrap_or("unknown").to_string(),
            message: commit.message().unwrap_or("unknown").to_string(),
            changes: Vec::new(),
        };

        let my_commit = if commit.parent_count() > 1 {
            Commit {
                r#type: CommitType::Merge,
                ..default_commit
            }
        } else {
            let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&commit_tree), None)?;
            let file_changes = extract_from_diff(&diff)?;
            Commit {
                r#type: CommitType::Normal,
                changes: file_changes,
                ..default_commit
            }
        };

        let my_commit_json = serde_json::to_string(&my_commit)
            .map_err(|_e| git2::Error::from_str("Serde failed!"))?;

        println!("{}", my_commit_json);
    }

    Ok(())
}

fn main() {
    let args: Vec<String> = env::args().collect();

    match args.get(1) {
        None => eprintln!("usage: {} REPO_URL", args[0]),
        Some(repo_url) => {
            match extract_logs(repo_url) {
                Ok(()) => eprintln!("Complete"),
                Err(e) => eprintln!("Err {:?}", e),
            };
        }
    }

    ()
}
