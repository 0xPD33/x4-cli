# x4-cli

`x4-cli` puts books on an Xteink X4 e-reader over Wi-Fi.
It needs the open-source [CrossPoint Reader](https://github.com/crosspoint-reader/crosspoint-reader) firmware on the X4.

```console
$ x4-cli sync ~/Books --save
saved library = "/home/you/Books" in /home/you/.config/x4-cli/config.toml
added  /Books/Frank Herbert - Dune.epub
added  /Books/Sci-Fi/Ursula K. Le Guin - The Dispossessed.epub
sync: 2 added, 14 already on the X4
```

## What it does

- Finds the X4 on your network.
- Uploads books and gives each EPUB a readable name, `Author - Title.epub`, from its metadata.
- Skips books that are already on the X4.
- Syncs a local folder to the X4, subfolders included.
- Lists, deletes, downloads, renames and moves files.
- Handles a weak Wi-Fi signal: when the X4 drops off the network during an upload, `x4-cli` waits for it and tries again.

## Install

**Release binaries.** Download the archive for your system from the
[releases page](https://github.com/0xPD33/x4-cli/releases). The Linux binaries are static.

**Cargo.** You need Rust 1.88 or newer.

```sh
cargo install --git https://github.com/0xPD33/x4-cli
```

**Nix.**

```sh
nix run github:0xPD33/x4-cli -- status
```

## Before you start

The X4 serves files only while its File Transfer screen is open.

1. On the X4, open File Transfer and choose **Join a Network**. The X4 then joins your Wi-Fi.
2. Put your computer on the same network as the X4.
3. Run `x4-cli find --save`. This finds the X4 and remembers its address.

The X4 can only join 2.4 GHz Wi-Fi. A guest network often blocks traffic between devices, so do not use one.

```console
$ x4-cli find --save
192.168.1.50     crosspoint  via udp
saved host = "192.168.1.50" in /home/you/.config/x4-cli/config.toml

$ x4-cli status
firmware   CrossPoint
host       192.168.1.50
reachable  yes
version    1.5.0
device     X4
ip         192.168.1.50
wifi       STA (rssi -58 dBm)
free heap  58196 bytes
uptime     414 s
```

## Commands

| Command | What it does |
|---|---|
| `x4-cli status` | Shows the firmware, the address and the Wi-Fi signal. |
| `x4-cli ls [folder]` | Lists a folder on the X4. `-l` shows sizes, `--json` prints JSON. |
| `x4-cli add <files...>` | Uploads books. |
| `x4-cli sync [folder]` | Uploads every book in a local folder that the X4 does not have. |
| `x4-cli rm <names...>` | Deletes files. Names can be globs such as `"*Dune*"`. `-r` deletes folders. |
| `x4-cli pull <name>` | Downloads a file. |
| `x4-cli mv <name> <dest>` | Renames a file or moves it into a folder. |
| `x4-cli find` | Looks for the X4 on your network. |

Every command takes `--host <address>` and `--trace`. `--trace` prints each raw request and reply.

Names on the X4 are relative to the books folder, `/Books`. Start a name with `/` to use a different folder:

```sh
x4-cli add "My Book.epub"               # goes to /Books
x4-cli add notes.txt --to /Notes        # goes to /Notes
x4-cli ls /                             # lists the top of the SD card
x4-cli rm "*Dune*"                      # asks before it deletes
x4-cli pull "Frank Herbert - Dune.epub" -o ~/Downloads
x4-cli mv "old name.epub" "New Name.epub"
```

### Book formats

`x4-cli` uploads `.epub`, `.txt`, `.xtc`, `.xtch` and `.bmp` files. It does not convert other formats.

I tried to convert PDF files to EPUB, and some results were bad. A PDF fixes each line and page in place, so a converter must guess where each paragraph starts. Get a real EPUB instead. These sources sell or give away EPUBs without DRM:

- [Standard Ebooks](https://standardebooks.org/) and [Project Gutenberg](https://www.gutenberg.org/) for public-domain books.
- Kobo, for books marked "DRM-Free".
- Humble Bundle and Baen Ebooks.

The X4 cannot open books with DRM. This includes most Kindle, Apple Books and library loans.

### File names

- `x4-cli` names each EPUB `Author - Title.epub` from its metadata. `--raw-name` keeps the local name.
- Other files keep their local name.
- `x4-cli` removes the characters `\ / : * ? " < > |` because the SD card cannot store them. It also removes a leading dot, because the X4 hides dotfiles.
- `--no-spaces` writes `Frank_Herbert-Dune.epub` instead. To make this the default, set `no_spaces = true` in the config file.

### Duplicates

- If a file with the same name and size is on the X4, `x4-cli` skips it.
- If a file with the same name has a different size, `x4-cli` stops before it uploads anything. Use `--force` to replace the file.

### Sync

`x4-cli sync` takes your local books folder as its argument. Add `--save` once, and later runs use that folder:

```sh
x4-cli sync ~/Books --save      # remembers ~/Books
x4-cli sync                     # syncs ~/Books again
x4-cli sync ~/Comics --to /Comics
```

- Each local subfolder becomes a folder on the X4.
- `x4-cli` ignores hidden files. It lists the files that are not books and leaves them alone.
- `--delete` also deletes books on the X4 that are not in the local folder. `x4-cli` asks first, unless you add `-y`. It only deletes book formats, so firmware files such as `update.bin` stay.

### A weak Wi-Fi signal

The X4 can drop off the network during a transfer when its signal is weak. `x4-cli status` shows the signal and warns below -80 dBm.

When an upload breaks, `x4-cli` does this:

1. It waits up to 60 seconds for the X4 to come back.
2. It deletes the partial file on the X4.
3. It tries the upload again. It makes at most 3 attempts for each file.

If the X4 refuses one file, `x4-cli` reports it and uploads the rest. If the X4 does not come back, `x4-cli` stops. Run the same command again: `x4-cli` skips the books that are already on the X4.

A download fails after 30 seconds without data. `x4-cli` then deletes the partial local file.

## Configuration

`x4-cli find --save` and `x4-cli sync --save` write the config file and print its path. On Linux the path is `~/.config/x4-cli/config.toml`.

```toml
host = "192.168.1.50"     # the X4's address
dir = "/Books"            # the books folder on the X4
library = "~/Books"       # the local folder for `x4-cli sync`
no_spaces = false         # true: `Frank_Herbert-Dune.epub`
```

`x4-cli` finds the address in this order: `--host`, the `X4_HOST` environment variable, the config file, then discovery.

## Tested firmware

I tested `find`, `status`, `ls`, `add`, `mv` and `rm` on a real X4 with CrossPoint 1.5.0.

## How discovery works

`x4-cli find` runs these checks at the same time and waits up to 2 seconds:

- It sends `hello` to UDP port 8134. CrossPoint answers this. `x4-cli` sends a broadcast, and also sends to each address on your local /24 network, because a stateful firewall drops the answers to a broadcast.
- It looks up `crosspoint.local` with your system resolver (mDNS).
- It tries `192.168.4.1`, the CrossPoint hotspot.

## Security

The File Transfer server on the X4 has no password and uses plain HTTP. While the screen is open, anyone on the same network can read, change and delete the files on the X4. The CrossPoint hotspot is an open network. Close File Transfer when you are done.

`x4-cli` only talks to the X4. It sends no data to the internet.

## Troubleshooting

- **"can't reach the X4"**: Make sure the File Transfer screen is open and the X4 is on the same network as your computer.
- **Uploads stop or are slow**: Run `x4-cli status` and look at the signal. Move the X4 closer to the router.
- **Something looks wrong**: Run the command again with `--trace` to see the raw requests and replies.

## Development

```sh
nix develop    # or use Rust 1.88 or newer
cargo test
```

The tests run the real `x4-cli` binary against a fake X4 in [`tests/fake`](tests/fake/mod.rs). The fake speaks both firmware dialects and stores files in a temporary folder. Each test writes a transcript and the final state of the fake SD card to `target/tmp/e2e/<test>.txt`.

The CrossPoint protocol comes from its source, [`CrossPointWebServer.cpp`](https://github.com/crosspoint-reader/crosspoint-reader/blob/master/src/network/CrossPointWebServer.cpp). Its endpoint documentation is out of date: for example, uploads do not overwrite files.

## License

MIT. `x4-cli` is not affiliated with Xteink or the CrossPoint project.
