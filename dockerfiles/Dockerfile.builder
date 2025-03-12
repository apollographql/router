FROM registry.access.redhat.com/ubi9:latest

RUN dnf -y install git make openssh-clients ca-certificates sudo unzip jq wget gcc g++ elfutils

RUN wget https://github.com/mikefarah/yq/releases/latest/download/yq_linux_amd64 -O /usr/bin/yq && chmod +x /usr/bin/yq

# There is no elfutils-devel any longer, so fake a pkgconfig file... :)
ADD libdw.pc /usr/lib64/pkgconfig/libdw.pc
