use std::fmt::Write as _;
use std::fs;
#[cfg(any(unix, windows))]
use std::io::Write as _;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use anyhow::{Context, Result};
use chrono::{DateTime, Local, Utc};
use cowboy_workflow_engine::{RunReport, RunStatusDetail, RunStatusState, WorkflowRuntime};

use crate::app::card::{SemanticCard, SemanticCardSection};
use crate::app::events::semantic_workflow_event_card;
use crate::app::state::{TranscriptEntry, WorkflowEventProjector};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportResult {
    pub run_id: String,
    pub path: PathBuf,
    pub card_count: usize,
}

/// Export a run to the caller's current working directory for backwards-compatible manual use.
pub async fn export_run(runtime: &WorkflowRuntime, run_id: &str) -> Result<ExportResult> {
    export_run_to_directory(runtime, run_id, runtime.cwd(), write_complete_file).await
}

/// Export a terminal workflow's persisted semantic transcript to Cowboy's durable state directory.
///
/// Returns `None` for running or waiting workflows. Callers deliberately handle failures as a
/// warning so exporting never changes the workflow outcome.
pub async fn export_terminal_report(
    runtime: &WorkflowRuntime,
    report: &RunReport,
) -> Result<Option<ExportResult>> {
    if !matches!(
        RunStatusDetail::from_status(&report.run.status).state,
        RunStatusState::Completed | RunStatusState::Failed | RunStatusState::Cancelled
    ) {
        return Ok(None);
    }

    let export_dir = runtime.state_dir().join("exports");
    prepare_private_export_directory(&export_dir)?;
    export_run_to_directory(
        runtime,
        &report.run.id,
        &export_dir,
        write_private_complete_file,
    )
    .await
    .map(Some)
}

async fn export_run_to_directory(
    runtime: &WorkflowRuntime,
    run_id: &str,
    directory: &Path,
    write_file: fn(&Path, &[u8]) -> Result<()>,
) -> Result<ExportResult> {
    let run = runtime
        .load_run(run_id)
        .await
        .with_context(|| format!("failed to load run {run_id}"))?;
    let events = runtime
        .load_events(&run.id)
        .with_context(|| format!("failed to load events for run {}", run.id))?;

    let cards = projected_cards(&run.id, run.created_at, &run.original_request, events);
    let html = render_html(&run.id, &cards);
    let path = directory.join(export_filename(&run.id));
    write_file(&path, html.as_bytes())?;

    Ok(ExportResult {
        run_id: run.id,
        path,
        card_count: cards.len(),
    })
}

fn projected_cards(
    run_id: &str,
    created_at: DateTime<Utc>,
    original_request: &str,
    events: Vec<cowboy_workflow_engine::WorkflowEvent>,
) -> Vec<SemanticCard> {
    let mut entries = Vec::new();
    let mut projector = WorkflowEventProjector::default();
    for event in events {
        projector.project(&mut entries, event);
    }

    let mut cards = vec![request_card(run_id, created_at, original_request)];
    cards.extend(entries.into_iter().filter_map(|entry| match entry {
        TranscriptEntry::Workflow {
            event,
            agent_descriptor,
        } => Some(semantic_workflow_event_card(
            &event,
            agent_descriptor.as_deref(),
        )),
        TranscriptEntry::Card { .. } => None,
    }));
    cards
}

fn request_card(run_id: &str, created_at: DateTime<Utc>, request: &str) -> SemanticCard {
    let timestamp = created_at.with_timezone(&Local).format("%H:%M").to_string();
    SemanticCard {
        header: format!("{timestamp} · ◌ Request · ▶ {}", short_run_id(run_id)),
        sections: vec![SemanticCardSection {
            label: Some("Request".to_string()),
            text: request.to_string(),
        }],
    }
}

fn short_run_id(run_id: &str) -> &str {
    run_id.get(..8).unwrap_or(run_id)
}

fn export_filename(run_id: &str) -> String {
    let safe = run_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!(
        "cowboy-export-{}.html",
        if safe.is_empty() { "_" } else { &safe }
    )
}

fn prepare_private_export_directory(path: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        return windows_private::prepare_directory(path);
    }

    #[cfg(not(windows))]
    {
        fs::create_dir_all(path).with_context(|| {
            format!(
                "failed to create transcript export directory {}",
                path.display()
            )
        })?;
        #[cfg(unix)]
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).with_context(|| {
            format!(
                "failed to restrict transcript export directory {}",
                path.display()
            )
        })?;
        #[cfg(not(unix))]
        anyhow::bail!(
            "private automatic transcript exports are unsupported on this platform: {}",
            path.display()
        );
        Ok(())
    }
}

fn write_complete_file(path: &Path, content: &[u8]) -> Result<()> {
    replace_complete_file(path, content, |temp_path, content| {
        fs::write(temp_path, content)
            .with_context(|| format!("failed to write temporary export {}", temp_path.display()))
    })
}

fn write_private_complete_file(path: &Path, content: &[u8]) -> Result<()> {
    #[cfg(windows)]
    windows_private::restrict_existing_file(path)?;

    replace_complete_file(path, content, write_private_file)?;

    #[cfg(windows)]
    windows_private::verify_owner_only_dacl(path)?;

    Ok(())
}

fn replace_complete_file(
    path: &Path,
    content: &[u8],
    write_temp_file: impl FnOnce(&Path, &[u8]) -> Result<()>,
) -> Result<()> {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("export path has no valid filename")?;
    let temp_path = path.with_file_name(format!(".{filename}.tmp"));
    write_temp_file(&temp_path, content)?;
    if let Err(err) = fs::rename(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(err).with_context(|| format!("failed to replace export {}", path.display()));
    }
    Ok(())
}

#[cfg(unix)]
fn write_private_file(path: &Path, content: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("failed to write temporary export {}", path.display()))?;
    file.write_all(content)
        .with_context(|| format!("failed to write temporary export {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to restrict transcript export {}", path.display()))
}

#[cfg(windows)]
fn write_private_file(path: &Path, content: &[u8]) -> Result<()> {
    windows_private::write_new_private_file(path, content)
}

#[cfg(not(any(unix, windows)))]
fn write_private_file(path: &Path, _content: &[u8]) -> Result<()> {
    anyhow::bail!(
        "private automatic transcript exports are unsupported on this platform: {}",
        path.display()
    )
}

#[cfg(windows)]
mod windows_private {
    use std::fs;
    use std::io::Write as _;
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use std::path::Path;
    use std::ptr::null_mut;

    use anyhow::{Context, Result, bail};
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ALREADY_EXISTS, ERROR_INSUFFICIENT_BUFFER, GENERIC_ALL, GENERIC_WRITE,
        INVALID_HANDLE_VALUE, LocalFree,
    };
    use windows_sys::Win32::Security::Authorization::{
        GetNamedSecurityInfoW, SE_FILE_OBJECT, SetNamedSecurityInfoW,
    };
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, DACL_SECURITY_INFORMATION, EqualSid, GetAce,
        GetAclInformation, GetLengthSid, GetSecurityDescriptorControl, GetTokenInformation,
        InitializeAcl, InitializeSecurityDescriptor, PROTECTED_DACL_SECURITY_INFORMATION,
        SE_DACL_PROTECTED, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR, SECURITY_DESCRIPTOR_REVISION,
        SetSecurityDescriptorControl, SetSecurityDescriptorDacl, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_NONE,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    pub(super) fn prepare_directory(path: &Path) -> Result<()> {
        let parent = path
            .parent()
            .context("transcript export path has no parent directory")?;
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create transcript export parent directory {}",
                parent.display()
            )
        })?;

        let mut security = OwnerOnlySecurity::for_current_user()?;
        let attributes = security.attributes();
        let wide = wide_path(path);
        // SAFETY: `wide` is NUL-terminated and `attributes` remains valid throughout this call,
        // including the security descriptor and ACL to which it points.
        let created = unsafe { CreateDirectoryW(wide.as_ptr(), &attributes) };
        if created == 0
            && std::io::Error::last_os_error().raw_os_error() != Some(ERROR_ALREADY_EXISTS as i32)
        {
            return Err(std::io::Error::last_os_error()).with_context(|| {
                format!(
                    "failed to create transcript export directory {}",
                    path.display()
                )
            });
        }
        restrict_path(path, &security)
    }

    pub(super) fn restrict_existing_file(path: &Path) -> Result<()> {
        if path.exists() {
            let security = OwnerOnlySecurity::for_current_user()?;
            restrict_path(path, &security)?;
        }
        Ok(())
    }

    pub(super) fn write_new_private_file(path: &Path, content: &[u8]) -> Result<()> {
        let mut security = OwnerOnlySecurity::for_current_user()?;
        let attributes = security.attributes();
        let wide = wide_path(path);
        // SAFETY: `wide` is NUL-terminated and `attributes` remains valid throughout the
        // creation call. `CREATE_NEW` refuses an existing temporary file, so no inherited or
        // pre-existing DACL can expose transcript content before the ACL is verified.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                GENERIC_WRITE,
                FILE_SHARE_NONE,
                &attributes,
                CREATE_NEW,
                FILE_ATTRIBUTE_NORMAL,
                null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error()).with_context(|| {
                format!(
                    "failed to create private temporary export {}",
                    path.display()
                )
            });
        }

        // SAFETY: `handle` is an owned file handle returned by `CreateFileW` and is transferred
        // exactly once to `File`, which closes it on drop.
        let mut file = unsafe { fs::File::from_raw_handle(handle.cast()) };
        if let Err(error) = restrict_path(path, &security).and_then(|_| {
            file.write_all(content)
                .with_context(|| format!("failed to write temporary export {}", path.display()))
        }) {
            drop(file);
            let _ = fs::remove_file(path);
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn verify_owner_only_dacl(path: &Path) -> Result<()> {
        let security = OwnerOnlySecurity::for_current_user()?;
        verify_path_dacl(path, &security)
    }

    fn restrict_path(path: &Path, security: &OwnerOnlySecurity) -> Result<()> {
        let wide = wide_path(path);
        // SAFETY: `wide` is NUL-terminated, and the ACL pointer belongs to `security`, which
        // remains alive for this call. The protected DACL flags replace inherited ACL entries.
        let error = unsafe {
            SetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                security.acl(),
                null_mut(),
            )
        };
        if error != 0 {
            bail!(
                "failed to restrict transcript export {}: {}",
                path.display(),
                std::io::Error::from_raw_os_error(error as i32)
            );
        }
        verify_path_dacl(path, security)
    }

    fn verify_path_dacl(path: &Path, security: &OwnerOnlySecurity) -> Result<()> {
        let wide = wide_path(path);
        let mut dacl = null_mut();
        let mut descriptor = null_mut();
        // SAFETY: `wide` is NUL-terminated; all output pointers refer to initialized local
        // storage. Windows allocates `descriptor`, which is released with `LocalFree` below.
        let error = unsafe {
            GetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                &mut dacl,
                null_mut(),
                &mut descriptor,
            )
        };
        if error != 0 {
            bail!(
                "failed to inspect transcript export ACL {}: {}",
                path.display(),
                std::io::Error::from_raw_os_error(error as i32)
            );
        }

        let result = (|| -> Result<()> {
            if dacl.is_null() {
                bail!("transcript export has no DACL: {}", path.display());
            }
            let mut info = Default::default();
            // SAFETY: `dacl` points into the security descriptor returned above and `info` has
            // the exact `ACL_SIZE_INFORMATION` layout requested by `AclSizeInformation`.
            if unsafe {
                GetAclInformation(
                    dacl,
                    (&raw mut info).cast(),
                    size_of_val(&info) as u32,
                    windows_sys::Win32::Security::AclSizeInformation,
                )
            } == 0
            {
                return Err(std::io::Error::last_os_error())
                    .context("failed to inspect transcript export DACL");
            }
            if info.AceCount != 1 {
                bail!(
                    "transcript export DACL is not owner-only: {}",
                    path.display()
                );
            }
            let mut ace = null_mut();
            // SAFETY: `dacl` stays valid through this block and index zero is valid because the
            // verified ACE count is exactly one.
            if unsafe { GetAce(dacl, 0, &mut ace) } == 0 {
                return Err(std::io::Error::last_os_error())
                    .context("failed to inspect transcript export ACE");
            }
            // SAFETY: the sole ACE was constructed as `ACCESS_ALLOWED_ACE` in
            // `OwnerOnlySecurity::for_current_user`; `ace` points to that layout.
            let ace = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
            if ace.Mask != GENERIC_ALL
                // SAFETY: `SidStart` is the first byte of the variable-size SID stored in this ACE.
                || unsafe { EqualSid((&raw const ace.SidStart).cast_mut().cast(), security.sid()) } == 0
            {
                bail!(
                    "transcript export DACL is not owner-only: {}",
                    path.display()
                );
            }
            let mut control = 0;
            let mut revision = 0;
            // SAFETY: `descriptor` remains valid until `LocalFree` after this closure returns.
            if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0
            {
                return Err(std::io::Error::last_os_error())
                    .context("failed to inspect transcript export DACL protection");
            }
            if control & SE_DACL_PROTECTED == 0 {
                bail!(
                    "transcript export DACL permits inheritance: {}",
                    path.display()
                );
            }
            Ok(())
        })();
        // SAFETY: `descriptor` was allocated by `GetNamedSecurityInfoW` and is released once.
        unsafe { LocalFree(descriptor.cast()) };
        result
    }

    struct OwnerOnlySecurity {
        descriptor: SECURITY_DESCRIPTOR,
        acl: Vec<u8>,
        sid: Vec<u8>,
    }

    impl OwnerOnlySecurity {
        fn for_current_user() -> Result<Self> {
            let sid = current_user_sid()?;
            let acl_len =
                size_of::<ACL>() + size_of::<ACCESS_ALLOWED_ACE>() - size_of::<u32>() + sid.len();
            let mut acl = vec![0; acl_len];
            let mut descriptor = SECURITY_DESCRIPTOR::default();
            // SAFETY: `acl` is sized for one `ACCESS_ALLOWED_ACE` containing `sid`; descriptor
            // is initialized before use and all pointers remain valid for this function call.
            let initialized = unsafe {
                InitializeAcl(acl.as_mut_ptr().cast(), acl.len() as u32, ACL_REVISION) != 0
                    && windows_sys::Win32::Security::AddAccessAllowedAce(
                        acl.as_mut_ptr().cast(),
                        ACL_REVISION,
                        GENERIC_ALL,
                        sid.as_ptr().cast_mut().cast(),
                    ) != 0
                    && InitializeSecurityDescriptor(
                        (&raw mut descriptor).cast(),
                        SECURITY_DESCRIPTOR_REVISION,
                    ) != 0
                    && SetSecurityDescriptorDacl(
                        (&raw mut descriptor).cast(),
                        1,
                        acl.as_ptr().cast(),
                        0,
                    ) != 0
                    && SetSecurityDescriptorControl(
                        (&raw mut descriptor).cast(),
                        SE_DACL_PROTECTED,
                        SE_DACL_PROTECTED,
                    ) != 0
            };
            if !initialized {
                return Err(std::io::Error::last_os_error())
                    .context("failed to create owner-only transcript export DACL");
            }
            Ok(Self {
                descriptor,
                acl,
                sid,
            })
        }

        fn attributes(&mut self) -> SECURITY_ATTRIBUTES {
            SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: (&raw mut self.descriptor).cast(),
                bInheritHandle: 0,
            }
        }

        fn acl(&self) -> *const ACL {
            self.acl.as_ptr().cast()
        }

        fn sid(&self) -> *mut core::ffi::c_void {
            self.sid.as_ptr().cast_mut().cast()
        }
    }

    fn current_user_sid() -> Result<Vec<u8>> {
        let mut token = null_mut();
        // SAFETY: the current-process pseudo-handle is valid for `OpenProcessToken`; `token`
        // points to initialized local storage and is closed below after a successful open.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(std::io::Error::last_os_error())
                .context("failed to open process token for transcript export ACL");
        }
        let result = (|| -> Result<Vec<u8>> {
            let mut required = 0;
            // SAFETY: this probes the required token information size; `required` is valid output storage.
            unsafe { GetTokenInformation(token, TokenUser, null_mut(), 0, &mut required) };
            if std::io::Error::last_os_error().raw_os_error()
                != Some(ERROR_INSUFFICIENT_BUFFER as i32)
                || required == 0
            {
                return Err(std::io::Error::last_os_error())
                    .context("failed to size process token user information");
            }
            let mut user = vec![0; required as usize];
            // SAFETY: `user` has the exact size returned by the probe and remains valid while its
            // embedded `TOKEN_USER` and SID are copied.
            if unsafe {
                GetTokenInformation(
                    token,
                    TokenUser,
                    user.as_mut_ptr().cast(),
                    required,
                    &mut required,
                )
            } == 0
            {
                return Err(std::io::Error::last_os_error())
                    .context("failed to read process token user information");
            }
            // SAFETY: `user` was populated as `TOKEN_USER`; its SID pointer points into `user`.
            let sid = unsafe { (&*user.as_ptr().cast::<TOKEN_USER>()).User.Sid };
            let len = unsafe { GetLengthSid(sid) } as usize;
            if len == 0 {
                return Err(std::io::Error::last_os_error())
                    .context("failed to read process token user SID");
            }
            // SAFETY: `sid` points to `len` readable bytes per `GetLengthSid`.
            Ok(unsafe { std::slice::from_raw_parts(sid.cast(), len) }.to_vec())
        })();
        // SAFETY: `token` is a real handle returned by `OpenProcessToken` and is closed exactly once.
        unsafe { CloseHandle(token) };
        result
    }

    fn wide_path(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }
}

fn render_html(run_id: &str, cards: &[SemanticCard]) -> String {
    let mut html = String::new();
    write!(
        html,
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Cowboy run {}</title>
<style>
:root {{ color-scheme: light dark; font-family: ui-monospace, SFMono-Regular, Consolas, monospace; }}
body {{ margin: 0; background: #111827; color: #e5e7eb; }}
main {{ max-width: 1100px; margin: 0 auto; padding: 24px; }}
h1 {{ font: 600 1.35rem system-ui, sans-serif; margin: 0 0 16px; }}
.controls {{ display: flex; flex-wrap: wrap; gap: 8px; position: sticky; top: 0; padding: 12px 0; background: #111827; z-index: 1; }}
input, button {{ font: inherit; border: 1px solid #4b5563; border-radius: 6px; padding: 8px 10px; background: #1f2937; color: inherit; }}
input {{ flex: 1 1 320px; }}
#match-count {{ align-self: center; min-width: 8rem; }}
details {{ border: 1px solid #374151; border-radius: 8px; margin: 10px 0; background: #1f2937; }}
summary {{ cursor: pointer; padding: 12px 14px; font-weight: 600; white-space: pre-wrap; }}
.body {{ border-top: 1px solid #374151; padding: 4px 14px 14px; }}
section {{ margin-top: 12px; }}
h2 {{ margin: 0 0 6px; font-size: .85rem; color: #93c5fd; }}
pre {{ margin: 0; white-space: pre-wrap; overflow-wrap: anywhere; font: inherit; line-height: 1.45; }}
[hidden] {{ display: none !important; }}
</style>
</head>
<body>
<main>
<h1>Cowboy run {}</h1>
<div class="controls">
<input id="search" type="search" placeholder="Search cards" aria-label="Search cards">
<button id="expand-all" type="button">Expand all</button>
<button id="collapse-all" type="button">Collapse all</button>
<span id="match-count" aria-live="polite">{} cards</span>
</div>
<div id="cards">
"#,
        escape_html(run_id),
        escape_html(run_id),
        cards.len()
    )
    .expect("writing to String cannot fail");

    for card in cards {
        write!(
            html,
            "<details class=\"card\"><summary>{}</summary><div class=\"body\">",
            escape_html(&card.header)
        )
        .expect("writing to String cannot fail");
        for section in &card.sections {
            html.push_str("<section>");
            if let Some(label) = &section.label {
                write!(html, "<h2>{}</h2>", escape_html(label))
                    .expect("writing to String cannot fail");
            }
            write!(html, "<pre>{}</pre></section>", escape_html(&section.text))
                .expect("writing to String cannot fail");
        }
        html.push_str("</div></details>\n");
    }

    html.push_str(
        r#"</div>
</main>
<script>
(() => {
  const cards = Array.from(document.querySelectorAll('.card'));
  const search = document.getElementById('search');
  const count = document.getElementById('match-count');
  const update = () => {
    const query = search.value.toLocaleLowerCase();
    let matches = 0;
    cards.forEach(card => {
      const visible = !query || card.textContent.toLocaleLowerCase().includes(query);
      card.hidden = !visible;
      if (visible) matches += 1;
      card.open = Boolean(query && visible);
    });
    count.textContent = query ? `${matches} match${matches === 1 ? '' : 'es'}` : `${cards.length} cards`;
    if (!query) cards.forEach(card => { card.open = false; });
  };
  search.addEventListener('input', update);
  document.getElementById('expand-all').addEventListener('click', () => cards.forEach(card => {
    if (!card.hidden) card.open = true;
  }));
  document.getElementById('collapse-all').addEventListener('click', () => cards.forEach(card => {
    card.open = false;
  }));
})();
</script>
</body>
</html>
"#,
    );
    html
}

fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use cowboy_workflow_engine::{WorkflowEvent, WorkflowEventKind};

    async fn runtime_with_export_workflows(dir: &tempfile::TempDir) -> WorkflowRuntime {
        let workflow_dir = dir.path().join("workflows");
        fs::create_dir(&workflow_dir).unwrap();
        for (name, source) in [
            (
                "complete",
                r#"
                local start = step("start")
                start.run = function(ctx)
                  return action.status { status = "success", body = "done" }
                end
                return workflow("complete", start)
                "#,
            ),
            (
                "fail",
                r#"
                local start = step("start")
                start.run = function(ctx)
                  return action.fail { reason = "expected failure" }
                end
                return workflow("fail", start)
                "#,
            ),
            (
                "wait",
                r#"
                local start = step("start")
                start.run = function(ctx)
                  return action.ask_user { id = "approval", message = "Approve?" }
                end
                return workflow("wait", start)
                "#,
            ),
        ] {
            fs::write(workflow_dir.join(format!("{name}.lua")), source).unwrap();
        }

        let config = crate::AppConfig {
            state_dir: dir.path().join("state"),
            workflow_store: dir.path().join("state/data.db"),
            workflow_dirs: vec![workflow_dir],
            ..crate::AppConfig::default()
        };
        WorkflowRuntime::new(config.runtime_config(dir.path().to_path_buf()))
            .await
            .unwrap()
            .with_deterministic_selector()
    }

    #[tokio::test]
    async fn terminal_reports_export_privately_and_waiting_reports_do_not() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_with_export_workflows(&dir).await;

        let completed = runtime
            .start_run_with_workflow("complete", "complete request")
            .await
            .unwrap();
        let completed_export = export_terminal_report(&runtime, &completed)
            .await
            .unwrap()
            .expect("completed run should export");
        assert!(
            completed_export
                .path
                .starts_with(runtime.state_dir().join("exports"))
        );
        let first_html = fs::read(&completed_export.path).unwrap();
        let repeated_export = export_terminal_report(&runtime, &completed)
            .await
            .unwrap()
            .expect("repeated terminal export should succeed");
        assert_eq!(completed_export.path, repeated_export.path);
        assert_eq!(fs::read(repeated_export.path).unwrap(), first_html);

        let failed = runtime
            .start_run_with_workflow("fail", "failed request")
            .await
            .unwrap();
        assert!(matches!(
            failed.run.status,
            cowboy_workflow_core::RunStatus::Failed { .. }
        ));
        assert!(
            export_terminal_report(&runtime, &failed)
                .await
                .unwrap()
                .is_some()
        );

        let waiting = runtime
            .start_run_with_workflow("wait", "waiting request")
            .await
            .unwrap();
        assert!(
            export_terminal_report(&runtime, &waiting)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            !runtime
                .state_dir()
                .join("exports")
                .join(export_filename(&waiting.run.id))
                .exists()
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            assert_eq!(
                fs::metadata(runtime.state_dir().join("exports"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o077,
                0
            );
            assert_eq!(
                fs::metadata(completed_export.path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o077,
                0
            );
        }

        #[cfg(windows)]
        {
            windows_private::verify_owner_only_dacl(&runtime.state_dir().join("exports")).unwrap();
            windows_private::verify_owner_only_dacl(&completed_export.path).unwrap();
        }
    }

    #[tokio::test]
    async fn automatic_export_failure_preserves_terminal_run_status() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime_with_export_workflows(&dir).await;
        let completed = runtime
            .start_run_with_workflow("complete", "complete request")
            .await
            .unwrap();
        fs::write(runtime.state_dir().join("exports"), "not a directory").unwrap();

        assert!(export_terminal_report(&runtime, &completed).await.is_err());
        assert_eq!(
            runtime.load_run(&completed.run.id).await.unwrap().status,
            completed.run.status
        );
    }

    #[test]
    fn export_filename_is_safe_and_deterministic() {
        assert_eq!(
            export_filename("../run:123<script>"),
            "cowboy-export-___run_123_script_.html"
        );
        assert_eq!(
            export_filename("run-123_ok"),
            "cowboy-export-run-123_ok.html"
        );
        assert_eq!(export_filename(""), "cowboy-export-_.html");
    }

    #[test]
    fn html_is_collapsed_searchable_self_contained_and_escaped() {
        let cards = vec![SemanticCard {
            header: "Header <script> & \"quoted\"".to_string(),
            sections: vec![SemanticCardSection {
                label: Some("Body".to_string()),
                text: "line one\n</script> BODY_ONLY_SEARCH_TOKEN".to_string(),
            }],
        }];
        let html = render_html("run<&", &cards);

        assert!(html.contains("<details class=\"card\"><summary>"));
        assert!(!html.contains("<details open"));
        assert!(html.contains("Header &lt;script&gt; &amp; &quot;quoted&quot;"));
        assert!(html.contains("&lt;/script&gt; BODY_ONLY_SEARCH_TOKEN"));
        assert!(!html.contains("Header <script>"));
        assert!(html.contains("id=\"search\""));
        assert!(html.contains("id=\"match-count\""));
        assert!(html.contains("id=\"expand-all\""));
        assert!(html.contains("id=\"collapse-all\""));
        assert!(html.contains("toLocaleLowerCase"));
        assert!(!html.contains("src=\"http"));
        assert!(!html.contains("href=\"http"));
    }

    #[test]
    fn complete_file_replaces_existing_destination() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("export.html");
        write_complete_file(&path, b"first").unwrap();
        write_complete_file(&path, b"second").unwrap();

        assert_eq!(fs::read_to_string(path).unwrap(), "second");
        assert!(!dir.path().join(".export.html.tmp").exists());
    }

    #[test]
    fn projected_export_cards_preserve_order_and_coalesce_stream_updates() {
        let created_at = "2026-01-02T03:04:05Z".parse().unwrap();
        let events = vec![
            WorkflowEvent::new(
                "run-123",
                WorkflowEventKind::AgentResponse {
                    step_id: "start".to_string(),
                    content: "first response line\n".to_string(),
                },
            ),
            WorkflowEvent::new(
                "run-123",
                WorkflowEventKind::AgentResponse {
                    step_id: "start".to_string(),
                    content: "second response line".to_string(),
                },
            ),
            WorkflowEvent::new(
                "run-123",
                WorkflowEventKind::AgentToolCall {
                    step_id: "start".to_string(),
                    tool_call_id: "tool-1".to_string(),
                    title: "Inspect fixture".to_string(),
                    tool_kind: "read".to_string(),
                    status: "running".to_string(),
                },
            ),
            WorkflowEvent::new(
                "run-123",
                WorkflowEventKind::AgentToolCallUpdate {
                    step_id: "start".to_string(),
                    tool_call_id: "tool-1".to_string(),
                    title: "Inspect fixture".to_string(),
                    status: "completed".to_string(),
                    content: Some(serde_json::json!({
                        "output": "TOOL_UPDATE_SEARCH_TOKEN"
                    })),
                },
            ),
            WorkflowEvent::new("run-123", WorkflowEventKind::RunCompleted),
        ];

        let cards = projected_cards("run-123", created_at, "export request", events);
        assert_eq!(cards.len(), 4);
        assert!(cards[0].header.contains("Request"));
        assert_eq!(cards[0].sections[0].text, "export request");
        assert!(cards[1].header.contains("Agent response"));
        assert_eq!(
            cards[1].sections[0].text,
            "first response line\nsecond response line"
        );
        assert!(cards[2].header.contains("Inspect fixture"));
        assert!(cards[2].sections.iter().any(|section| {
            section.label.as_deref() == Some("Output")
                && section.text.contains("TOOL_UPDATE_SEARCH_TOKEN")
        }));
        assert!(cards[3].header.contains("Run completed"));
    }
}
