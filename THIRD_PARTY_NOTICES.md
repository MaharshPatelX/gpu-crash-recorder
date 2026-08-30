# Third-party components

## AMD ADLX SDK 1.5

The native AMD telemetry bridge is compiled against AMD's ADLX SDK. The pinned SDK source and AMD's license agreement are stored in `vendor/adlx/`; the application loads the ADLX runtime installed with AMD Software at run time.

## PresentMon 2.5.1 x64

The official PresentMon 2.5.1 x64 release executable is embedded so frame capture works without a separate install. It is verified against SHA-256:

```text
9bec3083069f58f911e6a512f4806db51a27bd096103087bc1d05ef54c80a191
```

PresentMon is Copyright (C) 2017-2024 Intel Corporation and licensed under the MIT license. See `vendor/presentmon/LICENSE.txt`.
