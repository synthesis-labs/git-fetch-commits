# Git Fetch Commits

Just a cool tool to point at a git repo and return a JSON formatted summary of all commits, including file names, commit messages, timestamp and some basic change metrics.

Maybe a handy tool if you wanted to perform large scale analysis of commit history across multiple git repositories.

# Usage

```
# Compile the project
$ cargo build

# Run
$ cargo run <remote_url_of_repo>

```

# Outputs

JSON output is directed to stdout, whereas progress & logging directed to stderr.

# Caveats

- Only supports remote repos for now. Probably need a switch to support local / filesystem type repos.

# Git Authentication

Currently supports SSL Agent based auth as well as Plaintext auth.