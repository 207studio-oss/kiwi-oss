import codecs
import io
import json
import os
import sys

decoder = io.IncrementalNewlineDecoder(codecs.getincrementaldecoder(sys.stdin.encoding)(sys.stdin.errors), True)
encoder = codecs.getincrementalencoder(sys.stdout.encoding)(sys.stdout.errors)
if os.name != "nt":
    try:
        encoder.setstate(0)
    except Exception:
        pass
for line in sys.stdin.buffer:
    request = json.loads(line)
    try:
        if request["operation"] == "decode":
            result = decoder.decode(bytes(request["bytes"]), request["final"])
        else:
            result = list(encoder.encode((request["text"] + "\n").replace("\n", os.linesep)))
        reply = {"result": result}
    except Exception as error:
        reply = {"error": str(error)}
    sys.stdout.buffer.write(json.dumps(reply).encode("ascii") + b"\n")
    sys.stdout.buffer.flush()
