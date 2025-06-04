function setup_git
    read --prompt-str "Enter your Git user name: " git_user
    read --prompt-str "Enter your Git email: " git_email
    read --prompt-str "Do you want to configure Git credentials helper? (y/N): " use_creds

    git config --global user.name "$git_user"
    git config --global user.email "$git_email"

    if test (string match -r '^[Yy]' -- $use_creds)
        read --prompt-str "Enter your Git username (for HTTPS auth): " gh_user
        read --prompt-str "Enter your Git password or PAT: " --silent gh_pass

        git config --global credential.helper store

        echo "https://$gh_user:$gh_pass@github.com" > ~/.git-credentials
        chmod 600 ~/.git-credentials
    end

    echo "Git is configured. Welcome to 2009."
end
