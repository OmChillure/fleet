#!/bin/sh
# Read-only install check for an offline self-hosted runner.
# Does not start, stop, enable, pair, or print the machine token.
#
# Offline on Runners → Self-hosted means no heartbeat for 90s. The control
# plane cannot tell a deleted install from a disabled service. Run this on
# the computer that owns the card.
#
# verdict:
#   not_installed     nothing from the installer is on disk
#   bins_missing      config or service files remain; fleet-host is gone
#   service_disabled  installed, unit will not start at login
#   service_stopped   installed and enabled (or no unit); process is down
#   service_running   process is up (the card is a reachability problem)
set -eu

cfg_dir="${FLEET_HOST_CONFIG_DIR:-${HOME}/.config/fleet-host}"
user_bin="${HOME}/.local/bin/fleet-host"
root_bin="/usr/local/bin/fleet-host"
user_unit="${XDG_CONFIG_HOME:-${HOME}/.config}/systemd/user/fleet-host.service"
system_unit="/etc/systemd/system/fleet-host.service"
launchd="${HOME}/Library/LaunchAgents/ai.agentfleet.host.plist"
rcd="/usr/local/etc/rc.d/fleet-host"

say() { printf '%s\n' "$*"; }

bin=""
if [ -x "$user_bin" ]; then
    bin=$user_bin
elif [ -x "$root_bin" ]; then
    bin=$root_bin
elif command -v fleet-host >/dev/null 2>&1; then
    bin=$(command -v fleet-host)
fi

config_present=0
if [ -f "$cfg_dir/config.toml" ]; then
    config_present=1
fi

token_present=0
if [ -f "$cfg_dir/machine.token" ]; then
    _tok=$(sed 's/^[[:space:]]*//;s/[[:space:]]*$//' "$cfg_dir/machine.token" 2>/dev/null || true)
    if [ -n "$_tok" ]; then
        token_present=1
    fi
    unset _tok
fi

scope=""
unit_file=""
if [ -f "$user_unit" ]; then
    scope=user
    unit_file=$user_unit
fi
if [ -f "$system_unit" ]; then
    if [ "$(id -u)" -eq 0 ] || [ -z "$scope" ]; then
        scope=system
        unit_file=$system_unit
    fi
fi

enabled=""
active=""
sc_err=0
if [ -n "$scope" ] && command -v systemctl >/dev/null 2>&1; then
    if [ "$scope" = user ]; then
        _en_rc=0
        enabled=$(systemctl --user is-enabled fleet-host 2>/dev/null) || _en_rc=$?
        _ac_rc=0
        active=$(systemctl --user is-active fleet-host 2>/dev/null) || _ac_rc=$?
    else
        _en_rc=0
        enabled=$(systemctl is-enabled fleet-host 2>/dev/null) || _en_rc=$?
        _ac_rc=0
        active=$(systemctl is-active fleet-host 2>/dev/null) || _ac_rc=$?
    fi
    # is-enabled: 0 enabled, 1 disabled. Anything else did not answer.
    if [ "$_en_rc" -gt 1 ]; then
        sc_err=1
        enabled=""
    fi
    if [ "$_ac_rc" -gt 3 ]; then
        sc_err=1
        active=""
    fi
    unset _en_rc _ac_rc
fi

launchd_running=0
if [ -f "$launchd" ] && command -v launchctl >/dev/null 2>&1; then
    _uid=$(id -u)
    if launchctl print "gui/${_uid}/ai.agentfleet.host" >/dev/null 2>&1; then
        launchd_running=1
    fi
    unset _uid
fi

proc=0
if command -v pgrep >/dev/null 2>&1; then
    if pgrep -x fleet-host >/dev/null 2>&1; then
        proc=1
    fi
fi

running=0
if [ "$active" = active ] || [ "$active" = activating ] || [ "$proc" -eq 1 ] || [ "$launchd_running" -eq 1 ]; then
    running=1
fi

leftover=0
if [ "$config_present" -eq 1 ] || [ -n "$unit_file" ] || [ -f "$launchd" ] || [ -f "$rcd" ]; then
    leftover=1
fi

verdict=not_installed
code=4
next="On Runners → Self-hosted, use Add worker and paste the install command on this computer."
if [ -z "$bin" ] && [ "$leftover" -eq 0 ]; then
    verdict=not_installed
    code=4
elif [ -z "$bin" ]; then
    verdict=bins_missing
    code=4
    if [ "$token_present" -eq 1 ]; then
        next="The program is gone and the machine token is still here. Re-run the install command from Add worker; it can reinstall without a new pair token. Do not re-pair."
    else
        next="The program is gone. On Runners → Self-hosted, use Add worker and paste the install command on this computer."
    fi
elif [ "$running" -eq 1 ]; then
    verdict=service_running
    code=0
    next="This computer is running fleet-host. If the card stays offline, the process is not reaching the control plane. Read last_heartbeat below and the url in the config. Do not re-pair."
elif [ "$enabled" = disabled ] || [ "$enabled" = masked ] || [ "$enabled" = disabled-runtime ]; then
    verdict=service_disabled
    code=2
    if [ "$scope" = user ]; then
        next="systemctl --user enable --now fleet-host"
    elif [ "$scope" = system ]; then
        next="systemctl enable --now fleet-host"
    else
        next="Start fleet-host and enable it for login."
    fi
elif [ -n "$unit_file" ] || [ -f "$launchd" ] || [ -f "$rcd" ]; then
    verdict=service_stopped
    code=3
    if [ "$scope" = user ]; then
        next="systemctl --user start fleet-host"
    elif [ "$scope" = system ]; then
        next="systemctl start fleet-host"
    elif [ -f "$launchd" ]; then
        next="launchctl bootstrap gui/$(id -u) \"$launchd\""
    elif [ -f "$rcd" ]; then
        next="service fleet_host start"
    else
        next="\"$bin\" run"
    fi
else
    verdict=service_stopped
    code=3
    next="No service unit is installed. Start it in this terminal and leave it running: \"$bin\" run"
fi

if [ "$sc_err" -eq 1 ] && [ "$running" -eq 0 ] && [ -n "$bin" ]; then
    verdict=service_stopped
    code=3
    next="systemctl did not answer. Run this again from your login session. If it still fails: \"$bin\" run"
fi

say "fleet-host verify"
if [ -n "$bin" ]; then
    say "binary:          $bin"
else
    say "binary:          missing"
fi
if [ "$config_present" -eq 1 ]; then
    say "config:          $cfg_dir/config.toml"
else
    say "config:          missing"
fi
if [ "$token_present" -eq 1 ]; then
    say "token:           present"
else
    say "token:           missing"
fi
if [ -n "$scope" ]; then
    _en_show=$enabled
    _ac_show=$active
    [ -n "$_en_show" ] || _en_show=unknown
    [ -n "$_ac_show" ] || _ac_show=unknown
    say "service:         ${scope} systemd enabled=${_en_show} active=${_ac_show}"
    unset _en_show _ac_show
elif [ -f "$launchd" ]; then
    if [ "$launchd_running" -eq 1 ]; then
        say "service:         launchd running"
    else
        say "service:         launchd plist present, not running"
    fi
elif [ -f "$rcd" ]; then
    say "service:         rc.d script present"
else
    say "service:         no unit file"
fi
if [ "$proc" -eq 1 ]; then
    say "process:         running"
else
    say "process:         not running"
fi
hb="$cfg_dir/last_heartbeat.json"
if [ -f "$hb" ]; then
    say "last_heartbeat:  $(tr '\n' ' ' < "$hb")"
else
    say "last_heartbeat:  (none)"
fi
say "verdict:         $verdict"
say "next:            $next"
exit "$code"
