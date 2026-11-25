# Security model

## Credentials
* The EduPage password (only if "remember" is on), the EduPage session cookie, the GitHub token and AI API keys are stored **only** in Windows Credential Manager (service `EduTask`) through the `keyring` crate.
* SQLite stores no secrets. Logs never contain request bodies, tokens or passwords.
* Network errors are logged without their URLs, so query strings cannot leak.

## Running generated code (`src-tauri/src/engine/sandbox.rs`)
* Commands are a program plus an argument list, executed **without a shell**: no pipes, `&&` or redirection.
* Only an allowlist of compilers, interpreters and package managers may run, each with subcommand rules. For example, `npm publish`, `cargo install`, global installs, PowerShell, `cmd`, `curl` and `git` are refused.
* Package installation is a separate class of command and can be turned off in Settings.
* Arguments that are absolute paths or contain `..` are refused. The working directory is always `project/`.
* The environment is rebuilt from an allowlist. Variables whose names contain TOKEN, SECRET, PASSWORD, API_KEY and similar are never passed on. TEMP points inside the workspace.
* Every command has a timeout, after which the whole process tree is killed (`taskkill /T /F`).
* Downloaded attachments live in `materials/`, which the AI can read but not write, and nothing there is executed.

**Limitation:** this is process-level isolation, not a virtual machine. Generated programs run as your Windows user and can reach the network. To go further, run EduTask inside Windows Sandbox or a separate Windows account.

## Uploads (GitHub and EduPage)
* Files are selected through the project's `.gitignore`, plus hard exclusions (`.env*`, keys, certificates, `node_modules`, `target`, `bin/obj`, virtual environments, …).
* Every text file is scanned for token patterns (GitHub, Anthropic, AWS, Slack, Google, private keys) **and for your own stored credentials**. A match blocks the upload.
* Repositories are private by default.

## Submission
* The backend refuses to submit without a single-use confirmation token. The token is issued only by the review screen and expires after 15 minutes.
* A submission is "confirmed" only when EduPage returns an id for it. Manual hand-ins are labelled "self-reported".

## EduPage
* 2FA and CAPTCHA are never bypassed. The browser sign-in window shows EduPage's own page, has no access to EduTask's internal API, and is limited to https.
