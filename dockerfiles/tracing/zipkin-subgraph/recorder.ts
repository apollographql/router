const { BatchRecorder } = require("zipkin");
const { HttpLogger } = require("zipkin-transport-http");

module.exports.recorder = new BatchRecorder({
  logger: new HttpLogger({
    endpoint: "http://zipkin:9411/api/v2/spans"
  })
});