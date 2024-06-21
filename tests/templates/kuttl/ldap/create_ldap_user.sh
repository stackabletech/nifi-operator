#!/bin/sh

# To check the existing users
# ldapsearch -H ldap://localhost:1389 -D "cn=admin,dc=example,dc=org" -w admin -b "ou=my users,dc=example,dc=org"

# To check the new user
# ldapsearch -H ldap://localhost:1389 -D "cn=integrationtest,ou=my users,dc=example,dc=org" -w 'bindPasswordWithSpecialCharacter\@<&>"'"'" -b "ou=my users,dc=example,dc=org"

cat << 'EOF' | ldapadd -H ldap://localhost:1389 -D "cn=admin,dc=example,dc=org" -w admin
dn: ou=my users,dc=example,dc=org
ou: my users
objectclass: top
objectclass: organizationalUnit
EOF

cat << 'EOF' | ldapadd -H ldap://localhost:1389 -D "cn=admin,dc=example,dc=org" -w admin
dn: cn=integrationtest,ou=my users,dc=example,dc=org
objectClass: inetOrgPerson
objectClass: posixAccount
objectClass: shadowAccount
cn: integrationtest
uid: integrationtest
givenName: Stackable
sn: Integration-Test
mail: integrationtest@stackable.de
uidNumber: 16842
gidNumber: 100
homeDirectory: /home/integrationtest
loginShell: /bin/bash
userPassword: {crypt}x
shadowLastChange: 0
shadowMax: 0
shadowWarning: 0
EOF

ldappasswd -H ldap://localhost:1389 -D "cn=admin,dc=example,dc=org" -w admin -s 'bindPasswordWithSpecialCharacter\@<&>"'"'" "cn=integrationtest,ou=my users,dc=example,dc=org"
