# 0013: File roots come from what the person chose, never from the whole disk (2026-10-12)

**Status:** accepted. **Context:** `OpCtx::fs_access` (fail-closed file roots); spec v3 §6 (`fs:<path>` grants); the `io.import` and `io.export` ops.

## Context

`Grants::person()` holds every scope, including `fs`, but no file root, and `OpCtx::fs_access` needs both. So today even a person at the CLI cannot import a file from outside the project folder. Something has to add roots. Agents act through the same executor as the person, often in the same session, so whatever a person's session holds, an agent can reach unless the surface narrows it.

## Decision

1. **A root on the filesystem root (`/`, `C:\`, a whole drive) is never granted.** A surface that is asked for one refuses it with a message. Rejected because every agent acting in that session would inherit the whole disk, which is the opposite of §6.
2. **The project folder is a root for reads and writes** for people and agents alike, once the app opens a project (`Executor::with_project_dir`).
3. **A person adds roots by choosing a file or folder, and the root is exactly what they chose:**
   - **CLI:** each path argument the person typed (`--in`, `--out`, positional paths) adds a root for that file (or, for an output that does not exist yet, its parent folder). Paths inside scripts or op arguments sent by an agent add nothing.
   - **UI:** a file or folder picked in a native file dialog adds a root for that pick, for the rest of the session. Drag-and-drop counts as a pick.
   - **Saved roots:** `settings.fs_roots` lists folders the person added in Settings (for example an asset library on another drive). They persist with the project and are shown in Settings with a remove button.
4. **Agents get only the project folder plus roots the person delegates.** An agent session (MCP, the in-app assistant) does not inherit dialog or CLI roots. The person can delegate a saved root to agents with a per-session grant, shown in the session's grant list (§6).
5. **Roots are canonical real paths**: no symlinks, no `..`, no relative paths. A root reached through a symlink covers nothing. Writes always need a root, even inside the project folder; the project root counts.

## Threat model

**File grants confine ops; they are not a sandbox against processes that can already reach the file.** The boundary
is what ops do: no op creates a link, and every op opens files only through `OpCtx::fs_access` and the `CanonicalPath`
it returns, which refuses a symbolic link or junction at every step. A process outside that boundary that can
make a link already has the access the link would give: a hard link is the file itself, so a shell that can run `ln`
on a file can already read it, and Linux's `protected_hardlinks` stops anyone linking a file they cannot write. So an
op reads a hard link inside the project like the file it is. Two platform limits follow from relying on the file
system: drvfs and 9p (`/mnt/*` under WSL) report a link count of 1 for every file, so the guard that refuses to empty a
hard-linked file in place cannot see one there (project folders on `/mnt` are discouraged anyway: they are slow); and a
mount point is followed on Linux and macOS (a bind mount is a folder to the walk) but refused on Windows (a volume
mount point is a junction).

## Consequences

- Tasks, one per surface: CLI (plus the typed-path roots), UI file dialogs and Settings (UI phase), MCP sessions (agents get the project root only).
- A person never sees "permission denied" for a file they just picked, and an agent never reaches a file the person did not hand over.
- Rejected: home folder by default (an agent could read `~/.ssh`, browser profiles, tokens); option 2 above (whole disk).
- A root is also where opens start: an op opens a checked path only through `CanonicalPath`, which opens the
  project folder or the root it lies in and refuses a symbolic link or junction at every step below it, and
  `fs_access` follows a link only into places the request may reach. So rule 5 is enforced at open time too: on Windows
  a root must be the canonical path (a mapped drive's root as its UNC path), or opens under it are refused.
