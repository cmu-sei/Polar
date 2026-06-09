# TODO: I didn't realize nushell had logging in its standard lib when I made this,
# but now its already used heavily we should consider it deprecated until removed

# Private log fn, we label all logs with some level of importance
@deprecated "Use nushell's std/log module instead"
@category deprecated
def log [level: string, msg: string, --component: string = ""] {
    let ts = date now | format date "%Y-%m-%dT%H:%M:%S%.3fZ"
    print $"($ts) [($level)] ($component) — ($msg)"
}

export def log-info [msg: string, --component: string = ""] { log "INFO" $msg --component $component }
export def log-warn [msg: string, --component: string = ""] { log "WARN" $msg --component $component }
export def log-error [msg: string, --component: string = ""] { log "ERROR" $msg --component $component }
export def log-debug [msg: string, --component: string = ""] { log "DEBUG" $msg --component $component }
