# Overview

This test case configures the NiFi cluster to fetch Python and Java
processors via git-sync. A flow is deployed that include these
processors. The test job triggers the flow and checks the result.

# Custom processors

## Python processor "Greet"

The Greet processor is implemented in Python and answers a request with
the content "Hello!".

Only NiFi 2 supports Python components. In the flow of NiFi 1, the
Python processor is replaced with the internal ReplaceText processor.
NiFi 1 ignores the Python configurations.

## Java processor "Shout"

The Shout processor is implemented in Java and transforms the received
content to uppercase.

The processor is compiled with the dependencies for NiFi 2, but also
works in NiFi 1 if compiled with JDK 11.

# Flow

![NiFi canvas](./canvas.png)

1. The HandleHttpRequest processor listens for HTTP requests to
   `GET /greeting`. The request is forwarded to the Greet processor.
2. The Greet processor writes the content "Hello!" to the flow file that
   is then forwarded to the Shout processor.
3. The Shout processor transforms the content to uppercase. The flow
   file is forwarded to the HandleHttpResponse processor.
4. The HandleHttpResponse answers the HTTP request with this content.
