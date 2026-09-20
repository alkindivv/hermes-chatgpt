# Remote browser host

Remote browser host mode moves only the Electron/ChatGPT browser process to a Linux VPS. Codex, the
Responses bridge, MCP broker, project files, shell, Git, approvals, and tool execution remain on the
Mac.

This mode is intended for a trusted VPS you control. It does not expose CDP or the launcher control
server publicly: both continue to bind to `127.0.0.1` on the VPS and are carried to the Mac through
SSH local forwarding.

## Architecture

```text
Mac                                             VPS
--------------------------------------          -----------------------------
Codex / ChatGPT desktop                         Xvfb
project files                                   Electron launcher
shell / Git / local tools        SSH            ChatGPT browser session
Responses daemon                  |             127.0.0.1:<cdp>
MCP broker                        +----------->  127.0.0.1:<control>
browser helper
remote-browser link
```

The browser helper stays on the Mac. Only browser lifecycle, DOM automation, and CDP traffic cross
the SSH link. The VPS does not need a copy of the project.

## 1. Start the VPS browser host

Use a dedicated non-root Linux user. The launcher still needs a display server; Xvfb is sufficient
after the ChatGPT session has been signed in.

Example Debian/Ubuntu prerequisites:

```bash
sudo apt update
sudo apt install -y xvfb dbus-x11 libgtk-3-0 libnss3 libasound2t64
```

Start a private virtual display:

```bash
Xvfb :99 -screen 0 1440x1000x24 -nolisten tcp &
export DISPLAY=:99
```

When running this fork from source:

```bash
CODEX_CHATGPT_WEB_REMOTE_BROWSER_HOST_ONLY=1 bun run app
```

A packaged Linux launcher can instead be started with:

```bash
CODEX_CHATGPT_WEB_REMOTE_BROWSER_HOST_ONLY=1   ./codex-web-gpt-<version>-linux-x64.AppImage --hidden
```

For the first ChatGPT sign-in, expose the virtual display only through SSH/VNC/noVNC that you trust.
After the persistent launcher profile is authenticated, the graphical remote desktop is not required
for normal automatic turns.

The browser host writes its owner-only descriptor at:

```text
~/.codex-chatgpt-web/runtime/launcher-browser.json
```

Remote-browser-host-only mode does not start the local Responses daemon, MCP tunnel, or Codex route
on the VPS.

## 2. Connect the Mac to the VPS

Run this on the Mac from the matching fork/runtime:

```bash
codex-chatgpt-web remote-browser connect user@your-vps
```

The command:

1. reads the VPS launcher descriptor over SSH;
2. validates that CDP and control are loopback-only;
3. creates two SSH local forwards;
4. verifies CDP and the authenticated control endpoint;
5. writes an owner-only local descriptor; and
6. stays in the foreground to own the SSH link.

The local descriptor defaults to:

```text
~/.codex-chatgpt-web/runtime/remote-launcher-browser.json
```

Keep the command running while Codex uses the remote browser.

Optional paths:

```bash
codex-chatgpt-web remote-browser connect user@your-vps   --remote-descriptor '~/.codex-chatgpt-web/runtime/launcher-browser.json'   --local-descriptor "$HOME/.codex-chatgpt-web/runtime/remote-launcher-browser.json"
```

If the runtime cannot discover its local browser helper, pass an absolute local path with
`--browser-helper-script`.

### Why the forwarded CDP port is not remapped

Chromium's `/json/version` response advertises a WebSocket URL containing its actual debugging
port. The link therefore forwards the VPS CDP port to the **same port number** on the Mac. This
avoids rewriting CDP metadata and keeps Playwright's normal `connectOverCDP` path intact.

If that port is already occupied on the Mac, stop the conflicting local listener and reconnect.

## 3. Point the Mac runtime at the remote descriptor

For an existing full-harness installation, run setup while the remote-browser link is alive:

```bash
codex-chatgpt-web setup --full \
  --remote-browser-host-descriptor "$HOME/.codex-chatgpt-web/runtime/remote-launcher-browser.json" \
  --restart-service \
  --acknowledge-unofficial
```

The dedicated remote option is important: it keeps runtime ownership on the Mac. The Mac installs
or updates its local Responses daemon and tunnel service while using the launcher protocol only for
the browser living on the VPS. Do not substitute `--browser-host-descriptor`; that option retains
the upstream meaning that a local launcher owns the runtime.

Existing reusable tunnel credentials are preserved by normal setup behavior. Restart the local
Codex/ChatGPT desktop integration once after this ownership migration so the native model route is
reloaded.

The resulting authority boundary is:

```text
Mac owns:
- cwd and workspace roots
- project files
- shell commands
- Git
- MCP/tool execution
- approvals
- Responses daemon

VPS owns:
- Electron
- ChatGPT browser profile
- ChatGPT browser tabs
- rendering
- CDP/control endpoints
```

## Liveness and ownership

A normal launcher uses the browser-helper PID as local process-liveness evidence. That PID is
meaningless across machines, so remote turns explicitly identify themselves as remote owners.

For remote automatic turns, the VPS uses the existing authenticated heartbeat lease instead of
checking whether the Mac PID exists on Linux. Dynamic browser surface-to-CDP-target ownership is
refreshed from the authenticated `/v1/browser/descriptor` control endpoint before Playwright
attaches.

The local remote descriptor remains process-bound to the Mac-side link process. If the link exits,
its descriptor is removed.

## Security

- Do not bind Electron CDP or the launcher control server to `0.0.0.0`.
- Do not publish either port through a VPS firewall, reverse proxy, or public tunnel.
- Use SSH keys and a trusted VPS account.
- Treat the launcher profile and control token as sensitive authentication material.
- Keep the descriptor owner-only (`0600`); the link command enforces this.
- Use a dedicated VPS user instead of running Electron as root.
- This feature does not move or synchronize project files to the VPS.
