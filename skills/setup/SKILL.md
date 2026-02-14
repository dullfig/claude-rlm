# ClaudeRLM Setup

Use this skill to set up ClaudeRLM when the bootstrap script hasn't run yet or if you need to manually install the binary.

## Steps

1. Check if the binary exists at `${CLAUDE_PLUGIN_ROOT}/bin/claude-rlm`:

```bash
ls -la "${CLAUDE_PLUGIN_ROOT}/bin/claude-rlm" 2>/dev/null || echo "Not found"
```

2. If the binary is missing, run the bootstrap script:

**Unix (Linux/macOS):**
```bash
"${CLAUDE_PLUGIN_ROOT}/scripts/bootstrap"
```

**Windows:**
```cmd
"%CLAUDE_PLUGIN_ROOT%\scripts\bootstrap.cmd"
```

3. If bootstrap fails (network issues, etc.), download manually:

- Go to https://github.com/dullfig/claude-rlm/releases/latest
- Download the archive for your platform:
  - Linux x86_64: `claude-rlm-x86_64-unknown-linux-gnu.tar.gz`
  - macOS x86_64: `claude-rlm-x86_64-apple-darwin.tar.gz`
  - macOS ARM: `claude-rlm-aarch64-apple-darwin.tar.gz`
  - Windows: `claude-rlm-x86_64-pc-windows-msvc.zip`
- Extract the binary to `${CLAUDE_PLUGIN_ROOT}/bin/`
- On Unix, make it executable: `chmod +x ${CLAUDE_PLUGIN_ROOT}/bin/claude-rlm`

4. Verify the installation:

```bash
"${CLAUDE_PLUGIN_ROOT}/bin/claude-rlm" --version
```

5. Check memory status:

```bash
"${CLAUDE_PLUGIN_ROOT}/bin/claude-rlm" status
```
