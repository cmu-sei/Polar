FROM daveman1010220/polar-dev

# Create the user using the script
ARG USERNAME=vscode
ARG USER_UID=1000
ARG USER_GID=1000

# Run and remove the create-user.sh script
RUN sh create-user.sh $USERNAME $USER_UID $USER_GID
RUN rm create-user.sh

# Switch to the created user
USER $USERNAME