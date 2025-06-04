function fish_greeting --description="Displays the Fish logo and some other init stuff."
    set_color $fish_color_autosuggestion
    set_color normal
    echo 'The license for this container can be found in /root/license.txt' | dotacat
    lol "Welcome to the Polar Shell."
    
    # Array of funny phrases
    set phrases "Brace yourself for a flurry of brilliant code!" "Our code is cooler than an ice cube in Antarctica!" "Get ready to chill and code!" "Let's make some code that's ice-olated in perfection!" "There are no polar bears, only coding bears!" "Next stop: Bug-free code!" "Where the only thing frozen is the bugs!" "It's time to break the ice and dive into development!" "Where every line of code is as cool as the Arctic!" "Let's code like penguins on ice!" "Navigating the icy waters of code with ease!" "Our coding skills are as sharp as an icicle!" "Chill vibes, hot code!" "Frosty fingers, fiery code!" "Coding in a winter wonderland!" "Sliding into smooth code like a penguin!" "Our code is a polar express to success!" "Ice-cold focus, blazing fast code!" "From the tundra to triumph with our code!" "Arctic-level precision in every line!" "Coding through the polar vortex of bugs!" "Snow problem we can't solve with code!" "Where coding brilliance is as vast as the polar ice cap!" "Cool minds, warm hearts, perfect code!"

    # Select a random funny phrase
    set random_index (random 1 (count $phrases))
    set phrase $phrases[$random_index]

    echo $phrase | dotacat
end
