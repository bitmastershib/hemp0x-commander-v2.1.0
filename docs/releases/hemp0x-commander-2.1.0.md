# Hemp0x Commander 2.1.0

Hemp0x Commander 2.1.0 is a major feature update that marks the transition into a multi-chain wallet and decentralized exchange environment.

Back up `wallet.dat`, Hemp0x Vault files, and any important `hemp.conf` changes before upgrading.

## What Changed Since 2.0.1

- **Ravencoin Integration:** Added complete, non-custodial wallet support for the Ravencoin (RVN) network alongside Hemp0x.
- **Atomic Swaps (DEX):** Introducing decentralized, peer-to-peer Atomic Swaps. You can now securely swap native HEMP for native RVN entirely on-chain without any intermediaries or centralized exchanges.
- **Refined Dashboard UI:** The Commander UI has been meticulously polished with responsive navigation bars, a wide-format 1280x800 default window, and clear synchronization statuses to ensure you always know which network you are interacting with.
- **Removed Distractions:** Temporarily removed asset-related inputs from the swap wizard to ensure a razor-sharp focus on base-layer HEMP/RVN atomic swaps for this release.

## Bundled Daemons

- Hemp0x Core Next version: `v4.8.1.0-3e061497a`
- Ravencoin Core version: `v4.3.2.1` (or equivalent bundled version)
- Bundled binaries:
  - `hemp0xd`
  - `hemp0x-cli`
  - `ravend`
  - `raven-cli`

## Downloads

- Windows Portable: `Hemp0x_Commander_2.1.0_Windows_x64_Portable.zip`
- Windows Installer: `Hemp0x_Commander_2.1.0_x64_Setup.exe`
- Linux (Coming soon)

## Checksums

Final SHA256 checksums are published with the release artifacts in `SHA256SUMS.txt`. Verify the checksum before running.

Windows PowerShell:

```powershell
Get-FileHash .\Hemp0x_Commander_2.1.0_Windows_x64_Portable.zip -Algorithm SHA256
Get-FileHash .\Hemp0x_Commander_2.1.0_x64_Setup.exe -Algorithm SHA256
```

## Windows Portable

1. Extract `Hemp0x_Commander_2.1.0_Windows_x64_Portable.zip` to a writable folder.
2. Run `Hemp0x_Commander_2.1.0_x64_Portable.exe`.
3. Microsoft Edge WebView2 Runtime is required. Most Windows 10 and Windows 11 systems already include it.
4. The Windows build is unsigned. SmartScreen or antivirus products may warn on first launch. Verify the checksum before allowing the app.

## Windows Installer

1. Run `Hemp0x_Commander_2.1.0_x64_Setup.exe` to install the application.
2. Follow the wizard prompts.

## License

Hemp0x Commander is released under the MIT License.

Copyright (c) 2026 Hemp0x Devs

Bundled Core Next binaries, platform runtimes, and third-party dependencies keep their own licenses. See [Third-Party Notices](../THIRD_PARTY_NOTICES.md).
