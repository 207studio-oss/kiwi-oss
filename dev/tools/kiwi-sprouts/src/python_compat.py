import json
import sys


def tree(value):
    if isinstance(value, float):
        return {"float": json.dumps(value)}
    if isinstance(value, dict):
        return {"dict": [[key, tree(item)] for key, item in value.items()]}
    if isinstance(value, list):
        return {"list": [tree(item) for item in value]}
    return {"value": value}


request = json.loads(sys.stdin.buffer.read())
fields = request["fields"]
try:
    if request["operation"] == "parse":
        result = tree(json.loads(fields["text"]))
    elif request["operation"] == "encode":
        fields["text"].encode(fields["encoding"])
        result = None
    else:
        bytes(fields["bytes"]).decode(fields["encoding"])
        result = None
    reply = {"result": result}
except Exception as error:
    reply = {"error": str(error)}
sys.stdout.buffer.write(json.dumps(reply).encode("ascii"))
