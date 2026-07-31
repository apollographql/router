//> using scala 3.7.3
//> using platform scala-js
//> using jsModuleKind es

import scala.scalajs.js.annotation.JSExportTopLevel

object ScalaHeader:
  @JSExportTopLevel("headerValue")
  def headerValue(): String = "active"

  def main(args: Array[String]): Unit = ()
