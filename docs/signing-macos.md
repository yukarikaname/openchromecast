# Signing the macOS app

Three different things are called "macOS permissions" around mDNS, and which one you need depends on how the
app is distributed. This is the map, and then the two routes through it.

| | Who needs it | What it is |
|---|---|---|
| **Local Network permission** | everyone on macOS 15+ | a *user* prompt, declared with `NSLocalNetworkUsageDescription` + `NSBonjourServices` in `Info.plist` (already in `scripts/package-macos.sh`) |
| **`com.apple.developer.networking.multicast`** | **sandboxed** builds (Mac App Store) | an *entitlement* Apple grants per App ID, requested through the developer portal |
| **A provisioning profile** | any build that carries a managed entitlement | where Apple's grant is written down; it must be embedded in the bundle and named on the signing line |

The app discovers itself with **`mdns-sd`**, which speaks plain UDP multicast rather than Apple's Bonjour
API. That is the reason the multicast entitlement is in the picture at all: apps that use the Bonjour API
instead do not need it.

An **unsigned, unsandboxed** build needs none of the first two beyond the user prompt — that is what the
default release does today, and it works.

## What you need from the developer portal

You need a paid Apple Developer account. Then, at <https://developer.apple.com/account/resources>:

1. **Identifiers** → the App ID **`io.openchromecast.app`** (register it if it is not there) → enable the
   **Multicast Networking** capability on it. The entitlement must have been granted to the team first
   (Certificates, Identifiers & Profiles → the capability appears once Apple approves the request).
2. **Certificates** → **Developer ID Application** (for distribution outside the App Store) or
   **Apple Distribution** (for the App Store). Download it and export a `.p12` **with a password**.
3. **Profiles** → a **Developer ID** profile (or **Mac App Store** profile) for that App ID, including the
   certificate from step 2. Download the `.provisionprofile`.
4. Note the **Team ID** (10 characters, shown next to your team name).
5. For notarization: **App Store Connect → Users and Access → Integrations → App Store Connect API**.
   - If the page has no **Team Keys** tab yet, the **Account Holder** has to **Request Access** to the API
     first. It is one checkbox and a review, and nothing below works until it is approved.
   - Create a **Team Key**, not an Individual one. Apple's own words: individual keys "aren't able to use
     Provisioning endpoints, access Sales and Finance, or `notaryTool`". Creating a team key takes an
     **Account Holder** or **Admin**; give it at least the **Developer** role.
   - Keep the `.p8` (downloadable **once**), its **Key ID**, and the **Issuer ID** — the issuer is a
     team-key thing and `notarytool` requires it.

## Continuous integration (Developer ID)

Set these repository secrets (`gh secret set NAME < file`, or the repo's Settings → Secrets → Actions):

| Secret | What it is |
|---|---|
| `MACOS_SIGN_IDENTITY` | `Developer ID Application: Your Name (TEAMID)` — exactly as `security find-identity -v -p codesigning` prints it. This project's team is **`ZK98823LRA`**, the same one `package-macos.sh` already names as its default App Store identity. |
| `MACOS_CERT_BASE64` | the `.p12` from step 2, `base64 -i cert.p12` |
| `MACOS_CERT_PASSWORD` | the `.p12`'s password |
| `MACOS_PROVISION_PROFILE_BASE64` | the `.provisionprofile` from step 3, base64-encoded |
| `NOTARY_KEY_BASE64` | the `.p8`, base64-encoded |
| `NOTARY_KEY_ID` | the key's ID |
| `NOTARY_ISSUER_ID` | the issuer UUID |

Then run the **release** workflow by hand with **`macos_signing`** ticked. It imports the certificate,
decodes the profile, and `scripts/package-macos.sh` embeds the profile, signs with
`assets/Entitlements.developerid.plist` and the hardened runtime, notarizes and staples.

Without `MACOS_PROVISION_PROFILE_BASE64` the build still signs and notarizes, it just does not carry the
entitlement — which is correct: a Developer ID build without a profile that grants it cannot carry it.

## Locally, the same thing

```bash
cargo build --release --target aarch64-apple-darwin

SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)" \
PROVISION_PROFILE="$HOME/Downloads/OpenChromecast.provisionprofile" \
NOTARY_KEY_BASE64="$(base64 -i AuthKey_XXXXXXXXXX.p8)" \
NOTARY_KEY_ID=XXXXXXXXXX \
NOTARY_ISSUER_ID=00000000-0000-0000-0000-000000000000 \
  bash scripts/package-macos.sh dist/OpenChromecast-macos-arm64.zip
```

For the **Mac App Store** add `MAS=1`; that path sandboxes the app and uses
`assets/Entitlements.mas.plist` (app sandbox + network client/server + multicast), and App Store Connect
notarizes on upload rather than here.

## What ends up in the Actions log

No secret content, and that is by construction rather than by luck: the `.p12`, the profile and the `.p8` are
each written straight to a file with `base64 --decode > …`, so they are never on the log; the keychain password
is the literal `runnerpass`, a throwaway on an ephemeral runner; and there is no `set -x` in the workflow or in
the script, which is the one thing that would undo all of it.

What *is* printed, and is not secret:

- the signing identity, `>> signing with 'Developer ID Application: … (TEAMID)'` — the certificate's common
  name and the team, both of which are readable from any copy of the signed app anyway;
- the notary key's length and the issuer's last four characters, from the self-check whose whole purpose is to
  let a 401 be traced to one field. Apple documents both as identifiers rather than credentials; the `.p8` is
  the credential and it never appears;
- `PROVISION_PROFILE=<path>` — a path on the runner, not the profile.

If the identity line is unwelcome regardless, it is one `echo` in `scripts/package-macos.sh`.

## Checking what actually got signed

```bash
codesign -d --entitlements :- OpenChromecast.app      # the entitlement set that went in
codesign -d --verify --deep --strict -vv OpenChromecast.app
xcrun stapler validate OpenChromecast.app             # only after notarizing
ls OpenChromecast.app/Contents/embedded.provisionprofile   # present iff a profile was embedded
```

A managed entitlement with **no** embedded profile is the failure to watch for: signing succeeds, the
entitlement is listed, and macOS silently does not honour it.
