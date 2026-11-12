# Plugin protocol implementation

The public wire contract lives in the template's
[API reference](https://github.com/Among-Terminal/among-plugin-template/blob/main/docs/api.md).
Use these host implementation entry points when changing or testing that contract.
Host security and rendering constraints are in [plugin host development](plugins.md).

## Implementation sources

- [Managed messages, context and grants](../src/plugin/managed.rs),
  [host admission, replies and configuration](../src/plugin/host/managed_runtime.rs),
  [fragment validation](../src/plugin/protocol.rs),
  [slot storage and rendering](../src/slots.rs).
- [Manifest inspection](../src/plugin/manifest.rs),
  [settings schema](../src/plugin/schema.rs),
  [package catalogue](../src/plugin/install/metadata.rs),
  [entrypoint validation](../src/plugin/install/entrypoint.rs).
- [Repository installation](../src/plugin/install/source.rs),
  [archive validation](../src/plugin/install/package.rs),
  [local installation](../src/plugin/install/local.rs),
  [plugin manager](../src/prefs/plugin_manager.rs).
- [Sandbox rules](../src/plugin/sandbox/policy.rs),
  [sandbox launch](../src/plugin/sandbox/launch.rs),
  [protection levels](../src/plugin/sandbox/level.rs),
  [child environment](../src/plugin/host/environment.rs).
- [Notification bridge](../src/plugin/host/notification_backend.rs),
  [notification model](../src/plugin_notifications.rs),
  [notification limits](../src/notifications.rs).
- [Process transport and supervision](../src/plugin/process.rs),
  [frame limits](../src/plugin/transport.rs),
  [JSON validation](../src/plugin/data.rs),
  [host supervision](../src/plugin/host.rs),
  [application integration](../src/app.rs).
