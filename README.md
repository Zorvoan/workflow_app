# EduTask

Windows desktop app that synchronizes school assignments from **EduPage**, downloads their materials, uses AI to implement them, **builds and tests** the result in a controlled workspace, publishes it to **GitHub**, and prepares the **submission** back to EduPage, which you confirm explicitly.

Stack: **Tauri 2** (Rust backend, WebView2 UI) · **React + TypeScript + Vite** · **SQLite** · **Windows Credential Manager** for secrets.

---

## What works

| Area | Status |
|---|---|
| EduPage login (username/password) | ✅ Same flow as EduPage's website (via the community `edupage-api` research) |
| EduPage login with 2FA / CAPTCHA / SSO | ✅ "Sign in with browser": you log in on EduPage's own page; EduTask only reads the session cookie |
| Persistent session | ✅ Session cookie (and, if you opt in, your password) in Windows Credential Manager |
| Assignment sync (title, description, deadline, subject, teacher, attachments) | ✅ Automatic at a configurable interval, plus "Sync now" |
| New / changed assignment detection | ✅ Content hash; changed assignments are flagged "Updated" |
| Attachment download | ✅ EduPage-hosted files only; external links listed in `materials/links.md` |
| Assignment analysis → plan | ✅ Structured JSON plan (type, language, files, build/test commands) |
| BUILD ASSIGNMENT | ✅ Generate → install deps → build → test → automatic fix loop, with a live log |
| Build success reporting | ✅ Decided **only** from exit codes of commands EduTask runs itself |
| GitHub | ✅ Token or OAuth device flow, private by default, README + .gitignore, secret scan before upload |
| EduPage submission: message to teacher with files + repo link | ✅ Confirmed only when EduPage returns the new message id |
| EduPage submission: the homework "hand in" button | ⚠️ **Not automated**, see below. Guided manual hand-in, recorded as *self-reported* |

### Known limitations (stated plainly)

* **EduPage has no official public API for students.** The integration mirrors EduPage's web pages, based on the community-maintained [`edupage-api`](https://github.com/EdupageAPI/edupage-api) library. If EduPage changes its website, sync can break. All parsing lives in `src-tauri/src/edupage/parse.rs` so it can be fixed in one place. See [docs/EDUPAGE.md](docs/EDUPAGE.md).
* **The homework hand-in form is not automated.** It is undocumented, and guessing at it could produce a false "submitted". EduTask offers two honest options instead:
  1. **Send to teacher via EduPage message**: uploads the files to your EduPage cloud and messages the teacher. Verified by EduPage's response. This does *not* tick the homework as handed in.
  2. **Hand in on EduPage myself**: EduTask prepares the ZIP, copies the repo link, opens EduPage and the folder. After you hand it in, you mark it here, and it is recorded as *self-reported*.
* **The build sandbox is process-level, not a VM.** Commands run without a shell, from an allowlist, with a scrubbed environment, confined paths and timeouts. Generated code still runs as your Windows user and can use the network. See [SECURITY.md](SECURITY.md).
* Toolchains (g++, Python, .NET, JDK, Node, …) must be installed on your PC. Settings → Build lists what EduTask detected. It also spots the Microsoft Store `python.exe` alias, which is not a real Python install.

---

## Install (Windows)

**Prebuilt installer:** every push runs the *Windows build* GitHub Action. Download `edutask-windows-installer` from the run's artifacts and run the `.exe` (NSIS) or `.msi`. WebView2 is installed automatically if it is missing.

**Build it yourself:**

1. Install [Rust](https://rustup.rs) (MSVC toolchain), [Node.js 22](https://nodejs.org), and the "Desktop development with C++" workload of Visual Studio Build Tools.
2. Then run:

```bash
npm ci
```

```bash
npm run tauri build
```

The installers are written to `src-tauri/target/release/bundle/`. For development use `npm run tauri dev`.

## First run

1. **Settings → EduPage**: enter your school (`myschool` or `https://myschool.edupage.org`), username and password, then **Sign in**. If EduPage asks for 2FA or a CAPTCHA, use **Sign in with browser**.
2. **Settings → AI**: choose the provider and paste an API key (Anthropic is the default, using `claude-opus-5-5`). Any OpenAI-compatible endpoint, including a local model server, also works.
3. **Settings → GitHub**: paste a personal access token. Fine-grained tokens need *Administration* and *Contents* read/write; classic tokens need `repo`. Or set an OAuth App client id and use **Sign in with GitHub**.
4. **Settings → Build**: choose the project directory (default `Documents\EduTask`), the timeout, whether dependency installs are allowed, and which languages are allowed.
5. Open an assignment and press **BUILD ASSIGNMENT**. When the build succeeds, EduTask automatically:
   * pushes the project to GitHub (Settings → GitHub → "Push to GitHub automatically"),
   * prepares `submission\<name>.zip`,
   * copies the repository link to the clipboard and opens the GitHub repo and your EduPage in the browser,
   * shows a **Ready to hand in** panel with the link, the ZIP folder and an **I handed it in** button.

   Hand the assignment in on EduPage yourself, then click **I handed it in**. It is recorded as self-reported.

Each assignment gets a workspace:

```
Documents\EduTask\<subject-title-id>\
  materials\    downloaded attachments, assignment.md, links.md  (read-only for the AI)
  project\      the generated project (what goes to GitHub)
  submission\   ZIP and documents prepared for hand-in
  .edutask\     temp files
```

## Development

```bash
npm test
```

```bash
cd src-tauri && cargo test
```

Code map: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). Logs are written to `%APPDATA%\app.edutask.desktop\logs` (the exact path is shown under Settings → Appearance).
