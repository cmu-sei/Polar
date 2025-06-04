{ manpackage, ... }:

''
function man --description="Get the page, man"
    ${manpackage}/bin/man $argv | bat --language man --style plain
end
''
