# duviz

![Duviz screenshot](misc/example.png)

Duviz shows what is taking up space in a folder so you can clean up without guesswork. It draws a live, clickable map of your disk so big items stand out and you can move up and down the tree quickly.

It is written in Rust.

## How It Works (Plain Language)

Duviz looks at the folder you open it in and adds up the sizes of the things inside. Bigger things get bigger blocks on the screen. You can click a block to go into it and see what is inside, then keep drilling down until you find the space hogs.

## Features

- Clickable treemap view of disk usage
- Quick switch between folders view and files view
- Live scanning with progress
- One-click up navigation
- Right-click to delete files or folders (with confirmation)
- Shows the device you are on (like `/dev/sda1`)
- Displays the current version in the bottom-right corner

## Controls

- Click a block to enter a folder
- Click the `[Up]` label or press Backspace/Up/h to go up
- Press `f` to toggle folders/files view
- Right-click a block to delete it
- Press `q` to quit

## Install (Linux x86_64)

This installs the latest release to `/usr/local/bin/duviz` (run with sudo):

```
curl -fsSL https://raw.githubusercontent.com/hexajohnny/duviz/main/install.sh | sudo sh
```

## Build From Source

```
cargo build --release
sudo ./target/release/duviz
```

License: MIT OR Apache-2.0
