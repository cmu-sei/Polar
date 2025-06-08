{ cowsayPath, ... }:

''
function lol --description="lolcat (dotacat) inside cowsay"
    printf "%s\n" $argv | \
        cowsay -n -f (set cows (ls ${cowsayPath}/share/cowsay/cows); \
        set total_cows (count $cows); \
        set random_cow (random 1 $total_cows); \
        set my_cow $cows[$random_cow]; \
        echo -n $my_cow | \
        cut -d '.' -f 1) -W 79 | \
        dotacat
end
''
