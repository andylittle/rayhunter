# Uninstalling

There is no automated uninstallation routine, so this page documents the routine for some devices.

## Orbic

Run `./installer util orbic-shell --admin-password mypassword`. Refer to the
installation instructions for how to find out the admin password.

Inside, run:

```shell
echo 3 > /usrdata/mode.cfg  # only relevant if you previously installed via ADB installer
rm -rf /data/rayhunter /etc/init.d/rayhunter_daemon /bin/rootshell
reboot
```

Your device is now Rayhunter-free, and should no longer be rooted.

## TPLink

1. Run `./installer util tplink-shell` to obtain rootshell on the device.
3. `rm /data/rayhunter /etc/init.d/rayhunter_daemon`
4. `update-rc.d rayhunter_daemon remove`
5. (hardware revision v4.0+ only) In `Settings > NAT Settings > Port Triggers` in TP-Link's admin UI, remove any leftover port triggers.

## UZ801

0. (Optional): Back up the qmdl folder with all of the captures:
`adb pull /data/rayhunter/qmdl .`
1. Run `adb shell` to get a root shell on the device
2. Delete the /data/rayhunter folder: `rm -rf /data/rayhunter`
3. Modify the initmifiservice.sh script to remove the rayhunter 
startup line:
```sh
mount -o remount,rw /system
busybox vi /system/bin/initmifiservice.sh
```
Then type 999G (shift+g), then type dd. Then press the colon key (:) and type wq. Finally, press Enter.
4. Lastly, run `setprop persist.sys.usb.config rndis`.
5. Type `reboot` to reboot the device.

## Alcatel LinkZone MW41MP

0. (Optional): Back up the qmdl folder with all of the captures:
`adb pull /media/card/rayhunter/qmdl .`
1. Run `./installer util mw41-shell` to get a root shell on the device
2. Remove the rayhunter files and init script:
```sh
rm -rf /cache/rayhunter /media/card/rayhunter /etc/init.d/rayhunter_daemon /etc/rc5.d/S99rayhunter_daemon
reboot
```

Debug mode doesn't persist across a reboot, so the device is back to stock once it comes up.
