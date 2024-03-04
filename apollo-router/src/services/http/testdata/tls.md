# Certificate generation

## Server self signed certificate

```
openssl genrsa -out server.key 4096
openssl req -new -key server.key -out server_self_signed.csr
openssl x509 -req -in server_self_signed.csr -signkey server.key -out server_self_signed.crt -extfile server.ext
```

## Root certificate authority

```
openssl genrsa -out server.key 4096
openssl req -new -x509 -days 10000 -key ca.key -out ca.crt

You are about to be asked to enter information that will be incorporated
into your certificate request.
What you are about to enter is what is called a Distinguished Name or a DN.
There are quite a few fields but you can leave some blank
For some fields there will be a default value,
If you enter '.', the field will be left blank.
-----
Country Name (2 letter code) [AU]:FR
State or Province Name (full name) [Some-State]:.
Locality Name (eg, city) []:
Organization Name (eg, company) [Internet Widgits Pty Ltd]:Apollo GraphQL
Organizational Unit Name (eg, section) []:
Common Name (e.g. server FQDN or YOUR name) []:Apollo Test CA
Email Address []:
```

## Server certificate

```
openssl genrsa -out server.key 4096
openssl req -new -key server.key -out server.csr
openssl x509 -req -in server.csr -CA ./CA/ca.crt -CAkey ./CA/ca.key -out server.crt -CAcreateserial -days 10000 -sha256 -extfile server.ext
Certificate request self-signature ok
subject=C = FR, O = Apollo GraphQL, CN = router
```

## Client certificate

Generate the key:

```
openssl genrsa -out client.key.pem 4096
```

Certificate signing request:

```
openssl req -new -key client.key -out client.csr

You are about to be asked to enter information that will be incorporated   
into your certificate request.                                             
What you are about to enter is what is called a Distinguished Name or a DN.
There are quite a few fields but you can leave some blank                  
For some fields there will be a default value,                             
If you enter '.', the field will be left blank.                            
-----                                                                      
Country Name (2 letter code) [AU]:FR                                       
State or Province Name (full name) [Some-State]:.                          
Locality Name (eg, city) []:                                               
Organization Name (eg, company) [Internet Widgits Pty Ltd]:Apollo GraphQL  
Organizational Unit Name (eg, section) []:                                 
Common Name (e.g. server FQDN or YOUR name) []:router                      
Email Address []:                                                          
                                                                           
Please enter the following 'extra' attributes                              
to be sent with your certificate request                                   
A challenge password []:                                                   
An optional company name []:                                               
```

Generate the certificate:
```
openssl x509 -req -in client.csr -CA ./CA/ca.crt -CAkey ./CA/ca.key -out client.crt -CAcreateserial -days 10000 -sha256 -extfile client.ext
Certificate request self-signature ok
subject=C = FR, O = Apollo GraphQL, CN = router
```
