# Ember+ wire fixtures

Captured from node-emberplus client <-> provider on 127.0.0.1:9000.
All values are full S101 frames (0xFE ... 0xFF byte-stuffed, BER payload), hex-encoded.

## 1. Root GetDirectory REQUEST (client -> provider)

- frame[0] (32 bytes):

      fe000e0001c001021f0260106b0ea00c620aa003020120a1030201fddf948fff

Concatenated: `fe000e0001c001021f0260106b0ea00c620aa003020120a1030201fddf948fff`

## 2. Root GetDirectory RESPONSE (provider -> client)

- frame[0] (92 bytes):

      fe000e0001c001021f02604c6b4aa0486a46a0030d0100a13f313da0190c17456d6265725669657765725465737450726f7669646572a11b0c19456d62657256696577657220546573742050726f7669646572a3030101fddf0d4aff

Concatenated: `fe000e0001c001021f02604c6b4aa0486a46a0030d0100a13f313da0190c17456d6265725669657765725465737450726f7669646572a11b0c19456d62657256696577657220546573742050726f7669646572a3030101fddf0d4aff`

## 3. Child GetDirectory REQUEST - node "parameters" (path 0.1)

- frame[0] (46 bytes):

      fe000e0001c001021f02601e6b1ca01a6a18a0040d020001a210640ea00c620aa003020120a1030201fddf8c76ff

Concatenated: `fe000e0001c001021f02601e6b1ca01a6a18a0040d020001a210640ea00c620aa003020120a1030201fddf8c76ff`

## 4. Child GetDirectory RESPONSE (provider -> client)

- frame[0] (285 bytes):

      fe000e0001c001021f026082010a6b820106a0326930a0050d03000100a1273125a00a0c08696e74506172616da20302012aa303020100a403020164a503020103ad03020101a031692fa0050d03000101a1263124a00b0c097265616c506172616da20b090980011921fdd9f01b866ea503020101ad03020102a0356933a0050d03000102a12a3128a00d0c0b737472696e67506172616da20d0c0b68656c6c6f20656d626572a503020103ad03020103a0296927a0050d03000103a11e311ca00b0c09626f6f6c506172616da2030101fddfa503020101ad03020104a03b6939a0050d03000104a130312ea00b0c09656e756d506172616da203020101a503020103a7100c0e5265640a477265656e0a426c7565ad030201060689ff

Concatenated: `fe000e0001c001021f026082010a6b820106a0326930a0050d03000100a1273125a00a0c08696e74506172616da20302012aa303020100a403020164a503020103ad03020101a031692fa0050d03000101a1263124a00b0c097265616c506172616da20b090980011921fdd9f01b866ea503020101ad03020102a0356933a0050d03000102a12a3128a00d0c0b737472696e67506172616da20d0c0b68656c6c6f20656d626572a503020103ad03020103a0296927a0050d03000103a11e311ca00b0c09626f6f6c506172616da2030101fddfa503020101ad03020104a03b6939a0050d03000104a130312ea00b0c09656e756d506172616da203020101a503020103a7100c0e5265640a477265656e0a426c7565ad030201060689ff`

## 5. Decoded tree (for Rust assertions)

```json
{
  "elements": [
    {
      "nodeType": "QualifiedNode",
      "number": 0,
      "path": "0",
      "identifier": "EmberViewerTestProvider",
      "description": "EmberViewer Test Provider",
      "isOnline": true,
      "children": [
        {
          "nodeType": "QualifiedNode",
          "number": 0,
          "path": "0.0",
          "identifier": "identity",
          "isOnline": true
        },
        {
          "nodeType": "QualifiedNode",
          "number": 1,
          "path": "0.1",
          "identifier": "parameters",
          "isOnline": true,
          "children": [
            {
              "nodeType": "QualifiedParameter",
              "number": 0,
              "path": "0.1.0",
              "identifier": "intParam",
              "value": 42,
              "minimum": 0,
              "maximum": 100,
              "access": "readWrite",
              "type": "integer"
            },
            {
              "nodeType": "QualifiedParameter",
              "number": 1,
              "path": "0.1.1",
              "identifier": "realParam",
              "value": 3.14159,
              "access": "read",
              "type": "real"
            },
            {
              "nodeType": "QualifiedParameter",
              "number": 2,
              "path": "0.1.2",
              "identifier": "stringParam",
              "value": "hello ember",
              "access": "readWrite",
              "type": "string"
            },
            {
              "nodeType": "QualifiedParameter",
              "number": 3,
              "path": "0.1.3",
              "identifier": "boolParam",
              "value": true,
              "access": "read",
              "type": "boolean"
            },
            {
              "nodeType": "QualifiedParameter",
              "number": 4,
              "path": "0.1.4",
              "identifier": "enumParam",
              "value": 1,
              "access": "readWrite",
              "enumeration": "Red\nGreen\nBlue",
              "type": "enum"
            }
          ]
        }
      ]
    }
  ],
  "children": [
    {
      "nodeType": "QualifiedNode",
      "number": 0,
      "path": "0",
      "identifier": "EmberViewerTestProvider",
      "description": "EmberViewer Test Provider",
      "isOnline": true,
      "children": [
        {
          "nodeType": "QualifiedNode",
          "number": 0,
          "path": "0.0",
          "identifier": "identity",
          "isOnline": true
        },
        {
          "nodeType": "QualifiedNode",
          "number": 1,
          "path": "0.1",
          "identifier": "parameters",
          "isOnline": true,
          "children": [
            {
              "nodeType": "QualifiedParameter",
              "number": 0,
              "path": "0.1.0",
              "identifier": "intParam",
              "value": 42,
              "minimum": 0,
              "maximum": 100,
              "access": "readWrite",
              "type": "integer"
            },
            {
              "nodeType": "QualifiedParameter",
              "number": 1,
              "path": "0.1.1",
              "identifier": "realParam",
              "value": 3.14159,
              "access": "read",
              "type": "real"
            },
            {
              "nodeType": "QualifiedParameter",
              "number": 2,
              "path": "0.1.2",
              "identifier": "stringParam",
              "value": "hello ember",
              "access": "readWrite",
              "type": "string"
            },
            {
              "nodeType": "QualifiedParameter",
              "number": 3,
              "path": "0.1.3",
              "identifier": "boolParam",
              "value": true,
              "access": "read",
              "type": "boolean"
            },
            {
              "nodeType": "QualifiedParameter",
              "number": 4,
              "path": "0.1.4",
              "identifier": "enumParam",
              "value": 1,
              "access": "readWrite",
              "enumeration": "Red\nGreen\nBlue",
              "type": "enum"
            }
          ]
        }
      ]
    }
  ]
}
```
