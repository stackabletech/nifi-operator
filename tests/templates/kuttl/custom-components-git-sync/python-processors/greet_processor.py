from nifiapi.flowfiletransform import FlowFileTransform, FlowFileTransformResult


class Greet(FlowFileTransform):
    class Java:
        implements = ["org.apache.nifi.python.processor.FlowFileTransform"]

    class ProcessorDetails:
        version = "1.0.0"
        description = "A Python processor that greets politely."

    def __init__(self, **kwargs):
        pass

    def transform(self, context, flowfile):
        return FlowFileTransformResult(relationship="success", contents="Hello!")
