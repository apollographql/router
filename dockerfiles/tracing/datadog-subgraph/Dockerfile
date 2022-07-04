FROM node:16-bullseye

WORKDIR /src

ADD ./*.ts ./*.json /src/

RUN npm ci
RUN npx tsc

CMD [ "node", "dist/index.js" ]
