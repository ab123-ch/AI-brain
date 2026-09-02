# DingTalk Stream Transport

AI Brain can run an enterprise internal DingTalk robot directly from the local
Rust process. The process opens an outbound Stream WebSocket to DingTalk, so it
does not need a public IP, callback server, domain, certificate, or tunnel.

## Supported Scope

- One-to-one text messages to the application robot.
- Group text messages that explicitly mention the robot.
- One durable AI Brain room per DingTalk `conversationId` and robot.
- Existing AI Brain room history, default member, model policy, tools, and local
  working-directory access.
- Idempotent admission based on DingTalk `msgId`.
- Final text replies through the callback's temporary `sessionWebhook`.

The first release does not yet download files or images, render interactive
conversation cards, or fall back to DingTalk's proactive-send APIs after a
`sessionWebhook` expires. Keep the local process running until each answer is
delivered. Interactive New conversation / Switch conversation controls require
a DingTalk card template and are a separate follow-up.

## DingTalk Setup

1. Create an enterprise internal application in the DingTalk developer console.
2. Add the robot capability to that application.
3. Configure robot message receiving in Stream mode.
4. Publish the application to the users who may use it.
5. Copy the application's Client ID and Client Secret from its credentials page.

The Stream callback topic is fixed at `/v1.0/im/bot/messages/get`. AI Brain
subscribes to it automatically.

## Local Configuration

Configure the normal AI Brain LLM settings first. Then export the DingTalk
credentials in the terminal that starts AI Brain:

```bash
export DINGTALK_CLIENT_ID="ding..."
export DINGTALK_CLIENT_SECRET="..."

# Recommended after the robot is published. Comma-separated DingTalk staff IDs.
export DINGTALK_ALLOWED_STAFF_IDS="staff-id-1,staff-id-2"

cd rust
cargo run -p ai-brain-cli -- dingtalk
```

PowerShell:

```powershell
$env:DINGTALK_CLIENT_ID = "ding..."
$env:DINGTALK_CLIENT_SECRET = "..."
$env:DINGTALK_ALLOWED_STAFF_IDS = "staff-id-1,staff-id-2"

cd rust
cargo run -p ai-brain-cli -- dingtalk
```

Optional settings:

| Variable | Default | Meaning |
| --- | ---: | --- |
| `DINGTALK_REPLY_TIMEOUT_SECONDS` | `1800` | Maximum wait for one AI Brain final answer. |
| `DINGTALK_RECONNECT_SECONDS` | `5` | Delay before reconnecting Stream. |

If `DINGTALK_ALLOWED_STAFF_IDS` is empty, every user in the robot's DingTalk
publication scope can invoke local AI Brain tools. The process logs a warning in
that configuration. Prefer both a narrow DingTalk publication scope and an
explicit staff-ID allowlist.

## Runtime Behavior

```text
DingTalk message
  -> outbound Stream WebSocket already held by local AI Brain
  -> validate sender, mention, type, callback URL, and size
  -> create/reuse durable room and admit msgId transactionally
  -> ACK Stream callback
  -> existing collaboration member executes with normal tools
  -> poll authoritative room events for the final member reply
  -> POST reply to the temporary sessionWebhook
```

Credentials and session Webhooks are never written to source or AI Brain room
history. Logs contain short hashes for external message and conversation IDs,
not raw callback payloads or session URLs.

The command uses the current directory as the room working directory when a
DingTalk conversation creates its room for the first time. Start it from the
repository or directory that the robot should be allowed to inspect and modify.

## Operational Notes

- The computer must remain awake and online.
- Only outbound HTTPS and WSS access to DingTalk is required.
- Direct messages do not require an `@`; group messages do.
- Unsupported message types receive a short text explanation when the callback
  contains a valid DingTalk session Webhook.
- Repeated delivery of the same `msgId` does not start a second AI Brain run.
- If the local process restarts after admission but before reply delivery, the
  room task remains durable, but this first release does not persist the
  temporary Webhook for post-restart delivery.
