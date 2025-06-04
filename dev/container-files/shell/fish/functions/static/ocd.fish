# Inspired by a Fish plug-in for Mac OS, this will work on Ubuntu, possibly others.
function ocd --description="Open the current terminal directory in your default file manager."
    echo "This function needs updated for nixos."
    #set sys_name (uname)
    #if test "$sys_name" = 'Darwin'
        #open $PWD
    #else if test "$sys_name" = 'Linux'
        #xdg-open $PWD 2>&1 > /dev/null
    #end
end
