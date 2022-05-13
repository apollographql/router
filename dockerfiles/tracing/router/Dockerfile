FROM --platform=linux/amd64 ubuntu:latest
WORKDIR /usr/src/app
RUN apt-get update && apt-get install -y \
    libssl-dev \
    curl \
    jq

#COPY install.sh .
#RUN ./install.sh

COPY ./router /usr/src/app

STOPSIGNAL SIGINT


# Default executable is the router
ENTRYPOINT ["/usr/src/app/router"]