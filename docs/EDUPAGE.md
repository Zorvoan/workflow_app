# EduPage integration: research and decisions

## Is there an official API?

No API for students. EduPage (aSc Applied Software Consultants) offers integrations for school administrators (timetable exports, SSO, and data exchange with school information systems), but nothing that lets a student account read homework or hand it in programmatically. There is no published OAuth or REST API for this.

## What EduTask uses

The community-maintained Python library **edupage-api** (github.com/EdupageAPI/edupage-api, reviewed at v0.13.3) reproduces what EduPage's own website does. EduTask ports only the flows that library implements and tests. No endpoints were invented.

| Operation | Request | Code |
|---|---|---|
| Login (current) | `GET /login/?cmd=MainLogin`, RPC `akcia=getToken`, RPC `akcia=login`, follow `redirectUrl` | `client.rs::login_rpc` |
| Login (fallback) | `POST /login/edubarLogin.php` (`csrfauth`, `username`, `password`) | `client.rs::login_legacy` |
| RPC body encoding | `eqap = "dz:" + base64(raw deflate(form))`, `eqacs = sha1(eqap)`, `eqaz = 1` | `codec.rs` |
| Resume session | `PHPSESSID` cookie + `GET /user/` | `client.rs::resume` |
| Homework + lookups | `userhome({...})` JSON in `/user/`: `items[typ=homework]`, `dbi.subjects/teachers`, `userProps` | `parse.rs` |
| Older items | `POST /timeline/?module=todo&akcia=getData` (`datefrom`) | `client.rs::timeline_history` (best effort) |
| Upload file | `POST /timeline/?akcia=uploadAtt`, multipart `att` (50 MB EduPage limit) | `client.rs::upload` |
| Message | RPC `POST /timeline/?akcia=createItem` with `typ=sprava`; response `changes[0].timelineid` | `client.rs::send_message` |

Homework fields (`nazov` title, `popis` description, `date` deadline, `predmetid` subject, `superid` homework id, `vlastnik` teacher id) come from the same source. Several timeline items can describe one homework (created, then edited). They are grouped by `superid`, and the newest non-empty value of each field wins.

## Security rules

* **2FA and CAPTCHA are never bypassed.** When EduPage asks for either, the password login stops and the user is sent to "Sign in with browser", where they finish EduPage's own login page in a separate window. That window has no access to EduTask's internal API. Once the URL reaches `/user` on the school's host, EduTask reads only that host's `PHPSESSID` cookie and closes the window.
* Session cookies are sent only to `https://*.edupage.org`. Downloads that redirect elsewhere are refused.
* Passwords are stored only if the user opts in, and only in Windows Credential Manager.

## What cannot be automated reliably, and the fallback

**Handing in homework.** EduPage's homework hand-in goes through its e-learning/testing module. No public or community documentation of that flow exists. Automating it would mean guessing at an undocumented protocol, and it would risk showing "submitted" when EduPage did not record it. EduTask therefore:

1. Offers **"Send to teacher via EduPage message"** (documented flows: upload + createItem). This is recorded as *confirmed* only when EduPage returns the new message's `timelineid`.
2. Offers **guided manual hand-in**: it prepares the ZIP and documents, copies the repo link, opens EduPage and the folder. After the user hands the work in, it is recorded as *self-reported*, never as confirmed.

EduPage's "marked as done" flag (`userProps.doneMaxCas`) is shown for information only. It is a to-do checkbox, not proof of hand-in.

## When EduPage changes

* Sync fails with "unexpected response": check `parse.rs::extract_userhome` and the `homework_from_items` field names, and compare them with the current `edupage-api` release.
* Login fails for everyone: compare `client.rs::login_rpc` with `edupage-api`'s `login.py`.
* "Sign in with browser" keeps working through most changes, because it depends only on the `PHPSESSID` cookie and the `/user/` page.
