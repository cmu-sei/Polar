function export --description="Emulates the bash export command"
    if [ $argv ] 
        set var (echo $argv | cut -f1 -d=)
        set val (echo $argv | cut -f2 -d=)
        set -gx $var $val
    else
        echo 'export var=value'
    end
end
