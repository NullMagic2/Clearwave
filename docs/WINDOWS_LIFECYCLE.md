# Windows discovery and shutdown maintenance preview

A Windows 10 / Radeon RX 6600 report described delayed microphone discovery,
desktop stuttering, and difficulty closing the window. That machine has not
been reproduced locally; these changes address concrete code risks rather than
establishing the report's exact cause.

- Audio and Vulkan discovery run in independent, owned subprocesses. Microphone
  results no longer wait for graphics enumeration. Each discovery task has a
  15-second deadline and polls cancellation without blocking the UI thread.
- Endpoint ID/name failures become warnings while healthy endpoints remain in
  the list. Failure to enumerate one direction preserves the other direction.
- Closing a kill-on-close Windows job, terminating its process, waiting, and
  deleting its control file all happen on a reaper thread. Helper processes run
  below normal CPU priority. App shutdown cancels pending discovery tasks.
- A failed preferences write no longer prevents closing the main window.
- Controls detach app-owned fonts during teardown. The resource regression
  measures bounded GDI counts across repeated lifetimes instead of inspecting
  stale handles that Windows can cache or reuse.

Local checks cover good/bad endpoint mixtures, bounded JSON results, stalled
helper deadlines, cancellation, failed-save close, and repeated native window
lifetimes. Windows-target Clippy passes. Wine compatibility checks cannot
establish native Windows 10 driver behavior; the friend's retest remains needed.
The GPU requirements and audio processing model are unchanged.
