# Use an appropriate base image
FROM polar-dev

# Create the user using the script
ARG USERNAME=vscode
ARG USER_UID=1000
ARG USER_GID=1000

# Run and remove the create_user.sh script
RUN sh create_user.sh $USERNAME $USER_UID $USER_GID
RUN rm create_user.sh

# Switch to the created user
USER $USERNAME

