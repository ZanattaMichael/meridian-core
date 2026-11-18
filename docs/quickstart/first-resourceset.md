# Your first ResourceSet

A ResourceSet describes how one machine that already exists should be
configured. This page takes one from a document to a running web server.

## The document

[`examples/ansible/web-server.yaml`](../../examples/ansible/web-server.yaml):

```yaml
apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
spec:
  target: ansible
  targetHost:
    from: inline
    host: web01.example.com
  resources:
    - id: nginx_pkg
      type: package
      params:
        name: nginx

    - id: nginx_conf
      type: file
      params:
        path: /etc/nginx/conf.d/site.conf
        content: |
          server {
            listen 80;
            root /srv/www;
          }
        owner: root
        mode: "0644"
      dependsOn:
        - resource: nginx_pkg
      notifies:
        - resource: nginx_svc
          action: reload

    - id: nginx_svc
      type: service
      state: running
      params:
        name: nginx
        enabled: true
      dependsOn:
        - resource: nginx_pkg
```

Three resources: a package, a file and a service. The file comes after the
package (`dependsOn`), and when the file really changes the service is reloaded
(`notifies`). `target` says which tool does the work and `targetHost` says on
which machine.

## Check it

```
$ meridian validate examples/ansible/web-server.yaml
1 document valid
```

`validate` loads no plugin. It checks the document's shape: required fields,
ids that exist, and no dependency cycles (a cycle lists its resources). An
error names the file, line and field.

## Compile it

```
$ meridian compile examples/ansible/web-server.yaml --out build
web (ResourceSet, target ansible on web01.example.com)
  warning: ansible: resources "nginx_conf", "nginx_svc" are independent but will run in sequence; this target flattens the dependency graph and cannot express parallelism
  compiled 2 files into build/web
```

Nothing has changed anywhere. `build/web/playbook.yml` is an ordinary playbook
you could run by hand:

```yaml
---
- name: web
  hosts: web01.example.com
  tasks:
    - name: nginx_pkg
      ansible.builtin.package:
        name: nginx
        state: present
    - name: nginx_conf
      ansible.builtin.copy:
        content: |
          server {
            listen 80;
            root /srv/www;
          }
        dest: /etc/nginx/conf.d/site.conf
        mode: "0644"
        owner: root
      notify:
        - reload nginx_svc
    - name: nginx_svc
      ansible.builtin.service:
        enabled: true
        name: nginx
        state: started
  handlers:
    - name: reload nginx_svc
      ansible.builtin.service:
        enabled: true
        name: nginx
        state: reloaded
```

The warning is about the tool: the document says the file and service are
independent, but Ansible runs them in sequence anyway. Warnings never hide a
loss of meaning. When a tool cannot do what a document asks, the compile fails.

## Plan it

```
$ meridian plan examples/ansible/web-server.yaml
```

runs the tool's preview (`ansible-playbook --check --diff`) and reports what an
apply would change, per resource, by your ids. Ansible handles the connection;
pass it what it needs through `MERIDIAN_ANSIBLE_ARGS`:

```
$ MERIDIAN_ANSIBLE_ARGS="--user deploy --private-key ~/.ssh/deploy" \
    meridian plan examples/ansible/web-server.yaml
```

## Apply it

```
$ meridian apply examples/ansible/web-server.yaml
```

A second apply changes nothing: the tools converge on the state the document
describes.

## The same document, another tool

Change `target: ansible` to `target: chef` and compile again. The output is now
a cookbook, and the reload is Chef's own `notifies :reload`.

Change it to `target: puppet` and the compile fails:

```
web (ResourceSet, target puppet on web01.example.com)
  FAILED: web-server.yaml:37:11: puppet: resource "nginx_conf" [unsupported-action]: notifies resource "nginx_svc" with action "reload", which this target cannot express: Puppet's refresh restarts a service; it takes no argument and cannot be told to reload instead. Emitting a restart here would do more than the document asked for. ...
```

Puppet can only restart a service on a change, not reload it, and Meridian will
not silently swap one for the other. Either change the action to `restart` for
this target ([`examples/puppet/web-server.yaml`](../../examples/puppet/web-server.yaml))
or keep a tool that can reload. The
[capability matrix](../reference/capability-matrix.md) shows these differences
for every target.

Next: [a two-stage workflow](first-two-stage.md), where one tool creates the
machine and another configures it.
