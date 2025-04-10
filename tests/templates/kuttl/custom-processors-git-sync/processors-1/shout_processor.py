from nifiapi.flowfiletransform import FlowFileTransform, FlowFileTransformResult


class Shout(FlowFileTransform):
    class Java:
        implements = ["org.apache.nifi.python.processor.FlowFileTransform"]

    class ProcessorDetails:
        version = "1.0.0"
        description = "A Python processor that shouts the received."

    def __init__(self, **kwargs):
        pass

    def transform(self, context, flowfile):
        input = flowfile.getContentsAsBytes().decode("utf-8")
        output = input.upper()
        return FlowFileTransformResult(relationship="success", contents=output)
