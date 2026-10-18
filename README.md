# Codex Web GPUI

Rekonstruksi aplikasi Codex Web GPT dengan Rust, GPUI, dan CEF untuk macOS. Kode launcher, pengaturan, bridge Responses/MCP, dan pengelolaan proses berada dalam workspace Rust ini. CEF menjalankan situs ChatGPT.

**Status: build pengembangan. Kesetaraan seluruh fitur, tampilan, dan perilaku dengan aplikasi asal belum terbukti.**

Referensi yang dibekukan: `miuuyy/codex-chatgpt-web` v6.1.10, commit `8645a89e6a5fbe011e1bfbb0821a386ca7477ad5`. Spesifikasi: [docs/spec.md](docs/spec.md). Status lanjutan: [docs/handoff.md](docs/handoff.md).

## Struktur

| Lokasi | Fungsi |
| --- | --- |
| `crates/contracts` | State, kebijakan model/konteks, dan pencatatan penggunaan berdasarkan bukti pengiriman. |
| `crates/bridge` | Responses/SSE, MCP, katalog model, kepemilikan tool, pembatalan, dan checkpoint. |
| `crates/runtime` | Profil terpisah, jurnal konfigurasi, proses milik aplikasi, tunnel, dan login Chrome sementara. |
| `crates/browser` | Operasi pada target CEF milik aplikasi, pemilihan model/konektor, input, lampiran, sesi, dan hasil Markdown. |
| `crates/app` | Launcher GPUI: Browser, Setup, MCP, Activity, Limits, dan Settings. |
| `vendor/upstream`, `upstream.lock.json` | Sumber referensi tetap dan inventaris perubahan upstream. |

## Build dan pengujian

Checkout cabang rekonstruksi dari fork yang sudah ada:

```sh
git clone --branch rebuild/rust-gpui --recurse-submodules https://github.com/alkindivv/hermes-chatgpt.git Codex-Web-GPUI
cd Codex-Web-GPUI
git -C vendor/gpui-cef apply ../../tools/patches/gpui-cef-pin.patch
python3 tools/upstream.py check
```

Patch CEF lokal disimpan sebagai file yang diperiksa oleh lock; submodule tetap menunjuk revisi asal. Terapkan patch sekali setelah clone baru. Cache CEF/Ninja dan profil runtime tidak disertakan dalam Git. Direktori `evidence/` berisi bukti lokal dan detail mesin, sehingga dikecualikan dari push.

Gunakan dependensi yang sudah dikunci dalam `Cargo.lock`, revisi GPUI yang ditetapkan workspace, CEF pada `.cef`, dan Ninja pada `.build-tools/bin`. Script menggunakan satu Cargo job dan mode offline. Dependensi yang belum tersedia harus disiapkan sebelum menjalankan build.

```sh
python3 tools/build-native.py test
python3 tools/build-native.py bundle
python3 tools/upstream.py check
python3 tools/upstream.py cef-guard
python3 tools/upstream.py test
node tools/test-auth.cjs
```

Bundle terpisah berada di `dist/codex-web-gpui.app`, dengan identitas `dev.alkindi.codex-web-gpui`. Build tidak memasang atau mengganti aplikasi di `/Applications`.

Untuk membuka profil pemeriksaan terpisah dari direktori proyek:

```sh
open -n dist/codex-web-gpui.app --args --profile "$PWD/.runtime/manual-qa"
```

Profil default aplikasi adalah `~/.codex-web-gpui`. Integrasi Codex hanya menulis ke `codex-home` milik profil tersebut. Aplikasi ini tidak memindahkan sesi, cookie, atau konfigurasi aplikasi lama. Login, transfer passkey, pemasangan route, konfigurasi tunnel, dan autostart memerlukan tindakan pengguna pada aplikasi.

## Batas bukti

Tes Rust dan fixture transport membuktikan kasus yang diuji. Tes tersebut bukan bukti login akun nyata, eksekusi tool melalui konektor publik, kesamaan visual, atau kinerja UI. Verifikasi konektor Automatic memilih nama persis dan membersihkan composer tanpa mengirim pesan. Mode Manual memeriksa kesehatan runtime lokal; pengguna tetap memilih konektor pada setiap turn.

Implementasi sesi dan konektor dijelaskan dalam [launcher-session-connector-parity.md](docs/launcher-session-connector-parity.md). Verifikasi 19 Oktober: 184 tes Rust lulus; 19 kasus DOM lulus di WebKit dan CEF milik aplikasi. Bundle terpisah berhasil dibangun dan dibuka. Hasil, screenshot, lint yang tersisa, dan batas bukti dicatat dalam [handoff.md](docs/handoff.md).

## Pekerjaan yang masih terbuka

Beberapa fungsi upstream masih parsial: pemulihan personalization pada Temporary Chat, staging prompt besar untuk Bigger Context, eksekusi composer predictions dan skill attachments, otomatisasi persetujuan di browser, penjadwalan kesehatan/restart tunnel, migrasi seluruh versi jurnal, dan pembaruan biner aplikasi. Tampilan kontrol atau nilai pengaturan yang tersimpan tidak membuktikan seluruh efek runtime tersebut sudah diterapkan. Persetujuan dan eksekusi tool native tetap menjadi kewenangan harness Codex luar.

Penerimaan akhir juga memerlukan perbandingan visual/motion, input/fokus CEF, alur akun nyata, lampiran nyata, dan pengujian kinerja. Rincian per domain: [contracts](docs/contracts-parity.md), [bridge](docs/bridge-parity.md), [browser](docs/browser-parity.md), [runtime](docs/runtime-parity.md), [sign-in](docs/runtime-sign-in-parity.md), dan [Limits](docs/launcher-limits-parity.md).

## Mengikuti upstream

Jalankan pemeriksaan inventaris sebelum menerima perubahan. Periksa kandidat commit pada checkout terpisah, petakan perubahan ke implementasi Rust, lalu jalankan tes yang terkait. Jangan mengganti hash hanya untuk meluluskan pemeriksaan. Prosedur lengkap: [upstream-maintenance.md](docs/upstream-maintenance.md).

Notice lisensi sumber dipertahankan di `assets/UPSTREAM-LICENSE`, `assets/GPUI-LICENSE`, dan notice per crate. Bundle juga menyertakan kedua notice serta `upstream.lock.json`.
