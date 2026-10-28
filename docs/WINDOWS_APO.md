# Windows capture APO — development preview

Clearwave processes the existing microphone's shared Windows capture stream.
Other apps select the original microphone. No virtual microphone, virtual cable,
WASAPI render stream, or speaker-monitoring option is created. This removes
Clearwave's former microphone-to-playback feedback route. Hardware sidetone,
Windows “Listen to this device”, or monitoring in another app remain independent.

## Audio path

Physical microphone → Windows capture MFX → Clearwave APO → recording/calling app.
The APO passes 480-sample mono packets over a local named pipe to the desktop-owned
GPU worker. The worker uses the same model, noise strength, echo reduction and
voice balance as Linux. GPU execution and pipe I/O never run inside APOProcess.

The callback preallocates bounded history/queues at LockForProcess, uses no locks,
allocation or system calls, and returns the requested frame count. Samples carry
absolute positions. Output has a fixed 5,760-sample / 120 ms delay, reported by
GetLatency, including when processing is stopped. The first 120 ms is silence.
Missing, late or disconnected worker data is replaced with the matching delayed
dry signal. Returning processed data fades in over 10 ms. Excess transport backlog
is discarded so a cold GPU start cannot permanently strand processing behind live
input. Sequence gaps reset model and waveform state. Dry fallback is a continuity
mechanism, not a claim that every packet was denoised.

The initial format contract is 48 kHz float32, mono or stereo; stereo is downmixed
for processing and duplicated back to both channels. Dry fallback retains each
original channel. Other formats are rejected. Set the microphone's shared format
to 48 kHz before setup. RAW/exclusive capture and disabled audio enhancements can
bypass the effect. Default and communications modes are registered; other modes
are not promised. Up to three concurrent mode instances are served independently,
with separate model state and GPU resources.

## Setup and removal

Extract the complete Windows archive, including `clearwave_apo.dll` and
`setup-apo.ps1`. Select the microphone and click **Set up microphone processing…**.
The explicit setup requires administrator approval. Reopen recording applications
(or restart Windows if the audio graph remains loaded), enable microphone audio
enhancements, and click **Start**. A ready worker alone does not prove Windows
loaded the effect; check the signal meter while an app records that microphone.

The development script copies the DLL into a hash-specific directory under
Program Files, records the original registry values before changes, and attaches
only to the selected capture endpoint. Existing mode or legacy manufacturer APOs
cause setup to stop, rather than replace their processing. Driver-owned registry
permissions may also prevent setup; the script does not take ownership or weaken
permissions. Modern OEM SFX/EFX entries remain untouched.

To undo, run the following from the extracted directory in an elevated 64-bit
PowerShell, substituting the endpoint GUID printed during setup:

```powershell
.\setup-apo.ps1 -Action Uninstall -Endpoint '{endpoint-guid}'
```

Removal restores saved values only when they still match Clearwave's changes,
retaining the snapshot if another program changed them. Detach all microphones
before upgrading the DLL. Loaded DLLs and their hash directories can remain until
Windows is restarted. Registration does not change the default microphone or
start recording, and setup does not restart the Windows audio service.

## Deployment limits

This archive is an **unsigned development preview**, not a production driver
package. Protected audio hosts may refuse its DLL. The script does not enable
test-signing, disable protected audio, or change Secure Boot. Generic post-install
endpoint registration is not the same as an OEM/componentized, signed APO driver
package. Signed deployment, native Windows 10/11 audio-host execution, device
format coverage, page-residency/real-time compliance and stress testing still
need validation before general distribution. Existing OEM effects require a
supported integration/chaining design rather than replacing their registry entry.

Local tests cover packet validation, bounded concurrent queues, time-aligned
fallback, stale responses, reconnect, COM negotiation, in-place buffers and pipe
cancellation. Compatibility-host tests are useful code checks; they do not prove
that a native protected audiodg process will load this DLL or meet audio deadlines.

References: [Microsoft APO implementation](https://learn.microsoft.com/en-us/windows-hardware/drivers/audio/implementing-audio-processing-objects),
[APO deployment](https://learn.microsoft.com/en-us/windows-hardware/drivers/dashboard/deploying-audio-processing-objects).
