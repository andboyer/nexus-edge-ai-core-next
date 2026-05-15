# USB hot-plug — operator quickstart

## Linux (production)

1. Format a USB stick with a label starting with `NEXUS_`:

   ```sh
   sudo mkfs.ext4 -L NEXUS_VAULT /dev/sdX1
   ```

2. Install the udev rule:

   ```sh
   sudo install -m 0644 deploy/udev/99-nexus-usb.rules \
       /etc/udev/rules.d/99-nexus-usb.rules
   sudo udevadm control --reload-rules
   sudo udevadm trigger
   ```

3. Plug the stick in. systemd-mount will mount it at
   `/var/lib/nexus/clips/usb/NEXUS_VAULT/`. The engine's
   `usb_watch` task picks it up on the next 5s scan and surfaces it
   under the **Storage Admin → Hot tier → USB tiering** card.

4. To route new clips to that volume, set the preferred label in
   your `nexus.toml` and restart the engine:

   ```toml
   [runtime.clips]
   preferred_usb_label = "NEXUS_VAULT"
   ```

   In-flight clips finish at their original location — the routing
   change only affects the *next* `open()`.

## macOS (dev convenience)

macOS auto-mounts USB volumes at `/Volumes/<label>`. The engine
expects mounts under `<clips_dir>/usb/`, so symlink them in once:

```sh
mkdir -p ~/.local/share/nexus-clips
ln -s /Volumes ~/.local/share/nexus-clips/usb
```

…and point `runtime.clips.clips_dir` at
`~/.local/share/nexus-clips`. From there the same rule applies:
plug a `NEXUS_*`-labeled volume in, set
`preferred_usb_label`, restart.

## Hard invariants

* **In-flight clips finish where they started.** Detaching a USB
  volume mid-recording will produce an unreadable file (the
  hardware is gone), but the recorder will not silently migrate
  to a different tier — that would split the clip across two
  containers.
* **`hot_handle` records the choice.** New clips on USB stamp
  `motion_clips.hot_handle = "usb-<label>"`; local clips stamp
  `"local"`. The cold replicator + soft-evict + retention sweep
  treat both identically because everything resolves through the
  same `clips_dir.join(hot_path)` formula.
* **Removal is operator-driven.** Unplugging a stick triggers a
  `STORAGE_USB_DETACHED` bus event and removes the entry from the
  registry, but never deletes any files. Re-plugging the same
  label re-attaches automatically on the next scan.
