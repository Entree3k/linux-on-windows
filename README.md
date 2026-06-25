
# linux-on-windows

**linux-on-windows** is a collection of **Linux commands** reimplemented as small, standalone, Windows executables. No WSL, no Cygwin, no MSYS2, no virtual machine, just native `.exe` files you can drop anywhere on your `PATH` and run straight from PowerShell or CMD

## Why?

Windows is missing a lot of the small, sharp tools that make working in a Linux terminal so pleasant  `locate`, `nano`, `cat`, `tail`, `pkill`, `tcpdump`, `touch`

This project takes a different approach: **reimplement the tools themselves**, natively, in Rust. The result is a folder of tiny, fast, portable `.exe`'s that behave like the commands you already know with zero runtime dependencies.

>  **Why Rust? Because Windows is already a rusty piece of sh*t**

Linux CLI tools are designed around a simple idea that each tool does a job, and does it well. Instead of relying on one giant application, you combine small utilities together to solve problems. They communicate through standard input and output. Commands can be chained together with pipes (`|`) or redirected to files (`>`), turning a handful of simple tools into powerful workflows. A command can generate data, another can filter it, another can sort it, and another can save the result all in a single line.

## Installation

1. Head to the [**Releases**](https://github.com/Entree3k/linux-on-windows/releases) page.
2. Download the `.exe` files you want (or grab them all).
3. Drop them in a folder of your choice (e.g. `C:\Tools\`)
4. On Windows Start search for Environment Variables
5. Click on `Edit the system environment variables`
6. Click on `Environment Variables...`
7. Select `PATH` click `Edit`
8. Click on `New` and add the path of the folder e.g `C:\Tools`

```powershell
# Example: run a tool
cat myfile.txt
```

>  **Tip:** put the executables in one folder and add it to your `PATH` so you can call `cat`, `grep`, `htop`, etc. from anywhere.

## The Tools

### Files & Text

| Command | What it does |
|---------|--------------|
| `cat` | Print and concatenate files |
| `tac` | Print files in reverse, last line first |
| `head` | Show the first lines of a file |
| `tail` | Show the last lines of a file |
| `cut` | Extract columns/fields from text |
| `sort` | Sort lines of text |
| `uniq` | Filter out repeated lines |
| `wc` | Count lines, words, and bytes |
| `grep` | Search text using patterns |
| `diff` | Compare two files line by line |
| `echo` | Print text to the terminal |
| `ls` | List directory contents |
| `cp` | Copy files and directories |
| `mv` | Move or rename files |
| `rm` | Remove files and directories |
| `touch` | Create files / update timestamps |
| `stat` | Show detailed file metadata |
| `file` | Identify a file's type |
| `sfind` | Search for files in a directory tree (a `find` clone) |
| `locate`| Quickly find files by name |

### Compression & Archives

| Command | What it does |
|----------|--------------|
| `gzip` | Compress/decompress `.gz` files |
| `bzip2` | Compress/decompress `.bz2` files |
| `zip` | Create `.zip` archives |
| `unzip` | Extract `.zip` archives |

### System Monitoring

| Command | What it does |
|------------|--------------|
| `htop` | Interactive process viewer |
| `top` | Live process/CPU monitor |
| `iotop` | Monitor disk I/O by process |
| `iptop` | Monitor network traffic by connection |
| `free` | Show memory usage |
| `ps` | List running processes |
| `pgrep` | Find processes by name |
| `kill` | Send signals to / terminate processes |
| `lsof` | List open files and the processes using them |
| `watch` | Run a command repeatedly and watch its output |
| `neofetch` | Show a pretty system-info summary |

### Networking

| Command | What it does |
|-----------|--------------|
| `dig` | Query DNS records |
| `ss` | Inspect sockets and connections |
| `tcpdump` | Capture and inspect network packets |

### Misc

| Command | What it does |
|---------|--------------|
| `icat` | Display images directly in the terminal |
| `nano` | Simple terminal text editor |

## Building from source

Every tool is an independent Cargo project. You'll need the [Rust toolchain](https://rustup.rs/).

**Build a single tool:**

```powershell

cd cat

cargo build --release

# binary appears at target\release\cat.exe

```
  
**Build all of them at once** (PowerShell):

```powershell

Get-ChildItem -Directory | ForEach-Object {

Push-Location  $_.FullName

cargo build --release

Pop-Location

}
```

## Contributing

Found a bug or want to add another tool? Contributions are welcome open an issue or a pull request. Each new tool should be its own folder with a self-contained Cargo project, and it'll be picked up by the release workflow automatically.

## License

See the individual tool folders for license details.

---

*Made for everyone who misses Linux commands while on Windows* 🐧❤️🪟
