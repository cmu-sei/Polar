function lht --description="ls -alh, but show only files modified today"
    lh (find . -maxdepth 1 -type f -newermt (date +%Y-%m-%d) ! -newermt (date -d tomorrow +%Y-%m-%d))
end
