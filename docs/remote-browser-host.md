# Remote browser host

Remote browser host mode moves the Electron/ChatGPT browser process and its Playwright browser
helper to a Linux VPS. Codex, the Responses bridge, MCP broker, project files, shell, Git, approvals,
and tool execution remain on the Mac.

This mode is intended for a trusted VPS you control. It does not expose CDP or the launcher control
server publicly: both continue to bind to `127.0.0.1` on the VPS. SSH local forwarding keeps
maintenance/setup access private, while automatic turns use an SSH stdio channel to a VPS-local
browser helper.

## Architecture

```text
Mac                                             VPS
--------------------------------------          -----------------------------
Codex / ChatGPT desktop                         Xvfb
project files                                   Electron launcher
shell / Git / local tools        SSH            ChatGPT browser session
Responses daemon                  |             browser helper / Playwright
MCP broker                        +----------->  127.0.0.1:<cdp>
remote-browser link               +----------->  127.0.0.1:<control>
```

The browser helper runs beside Electron on the VPS. The Mac daemon talks to that helper through an
SSH stdio channel, while MCP/tool execution remains on the Mac. This keeps the high-volume,
latency-sensitive Playwright/CDP protocol local to the browser host; only compact helper protocol
frames, compiled prompt payloads, progress, and response deltas cross SSH. The VPS does not need a
copy of the project and never executes Codex project tools.

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

1. reads the VPS launcher descriptor and its owning user over SSH;
2. validates that CDP and control are loopback-only;
3. creates two SSH local forwards for health, setup, and maintenance;
4. records the launcher's VPS-local browser-helper command in the local descriptor;
5. verifies CDP and the authenticated control endpoint;
6. writes an owner-only local descriptor; and
7. stays in the foreground to own the SSH link.

Automatic Codex turns start a second, persistent SSH stdio channel for the browser helper. If the
SSH login user differs from the launcher descriptor owner, the helper command switches to the owner
with `runuser`. The SSH account therefore needs permission to become the launcher owner, or you
should connect directly as that owner.

The local descriptor defaults to:

```text
~/.codex-chatgpt-web/runtime/remote-launcher-browser.json
```

Keep the command running while Codex uses the remote browser.

### macOS LaunchAgent

After the foreground link has been verified once, macOS can own it permanently through launchd.
Stop the foreground `remote-browser connect` process first, then install the LaunchAgent:

```bash
codex-chatgpt-web remote-browser service install user@your-vps \
  --remote-descriptor /home/ubuntu/.codex-chatgpt-web/runtime/launcher-browser.json \
  --local-descriptor "$HOME/.codex-chatgpt-web/runtime/remote-launcher-browser.json"
```

The service uses `RunAtLoad` and `KeepAlive`, runs SSH in non-interactive BatchMode, recreates the
ephemeral local descriptor after login/reboot, and automatically restarts after a transient SSH
failure. The SSH account must therefore authenticate without a password prompt.

Lifecycle commands:

```bash
codex-chatgpt-web remote-browser service status
codex-chatgpt-web remote-browser service restart
codex-chatgpt-web remote-browser service stop
codex-chatgpt-web remote-browser service start
codex-chatgpt-web remote-browser service uninstall
```

Logs are stored under `~/.codex-chatgpt-web/logs/remote-browser.stdout.log` and
`remote-browser.stderr.log`.

Optional paths:

```bash
codex-chatgpt-web remote-browser connect user@your-vps   --remote-descriptor '~/.codex-chatgpt-web/runtime/launcher-browser.json'   --local-descriptor "$HOME/.codex-chatgpt-web/runtime/remote-launcher-browser.json"
```

`--browser-helper-script` remains available only as a local compatibility fallback. Normal remote
turns execute the helper advertised by the VPS launcher.

### Why the forwarded CDP port is not remapped

Chromium's `/json/version` response advertises a WebSocket URL containing its actual debugging
port. The maintenance link therefore forwards the VPS CDP port to the **same port number** on the
Mac. This avoids rewriting CDP metadata for health/setup operations. Real automatic turns keep
Playwright/CDP local to the VPS helper instead of streaming DOM automation through this forward.

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
- browser-helper protocol coordination

VPS owns:
- Electron
- ChatGPT browser profile
- ChatGPT browser tabs
- rendering
- browser helper / Playwright
- CDP/control endpoints
```

## Liveness and ownership

A normal launcher uses the browser-helper PID as local process-liveness evidence. The Mac-side
maintenance link still identifies itself as a remote owner because its PID is meaningless on Linux.
Automatic browser turns instead run their helper on the VPS as the launcher descriptor owner, so the
launcher uses the helper's real VPS PID and local heartbeat. Dynamic browser surface-to-CDP-target
ownership stays local to the VPS helper.

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
