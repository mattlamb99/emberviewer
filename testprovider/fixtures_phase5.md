# Ember+ Phase 5 wire fixtures (interop node 0.4)

Captured from a node-emberplus client <-> the provider on 127.0.0.1:9000.
All frames are full S101 frames (0xFE ... 0xFF, byte-stuffed, BER payload), hex-encoded.
BER tag legend: `[APP n]` = application class, `[n]` = context-specific class.

## 1. interop getDirectory RESPONSE (node "interop", path 0.4)

Contains the enumMap parameter (0.4.0), tilde-enum (0.4.1), gain slider (0.4.2),
level slider (0.4.3), offline node (0.4.4), offline param (0.4.5) and meters node (0.4.6).

- frame[0] (446 bytes):

      fe000e0001c001021f02608201ab6b8201a7a061695fa0050d03000400a1563154a00e0c0c656e756d4d6170506172616da20302010aa503020103ad03020101af336831a00e670ca0050c034f6666a103020100a00e670ca0050c034c6f77a10302010aa00f670da0060c0448696768a103020114a0446942a0050d03000401a1393137a0100c0e74696c6465456e756d506172616da203020100a503020103a7140c125265640a7e52657365727665640a426c7565ad03020106a0426940a0050d03000402a1373135a00c0c0a6761696e536c69646572a203020100a3030201c4a40302010ca503020103a6070c052564206442a803020101ad03020101a0446942a0050d03000403a1393137a00d0c0b6c6576656c536c69646572a20b090980fddf10000000000000a3020900a40b0909800010000000000000a503020103ad03020102a0216a1fa0050d03000404a1163114a00d0c0b6f66666c696e654e6f6465a303010100a031692fa0050d03000405a1263124a00e0c0c6f66666c696e65506172616da203020107a503020101a903010100ad03020101a01c6a1aa0050d03000406a111310fa0080c066d6574657273a3030101fddf062aff

- frame[1] (75 bytes):

      fe000e0001c001021f02603c653aa0146512a003020101a10b0909c004120ccccccccccda0146512a003020102a10b0909c0041568f5c28f5c29a00c650aa003020103a1030201eeaec5ff

- frame[2] (75 bytes):

      fe000e0001c001021f02603c653aa0146512a003020101a10b0909c0041ab851eb851eb8a0146512a003020102a10b0909c0031a99999999999aa00c650aa003020103a1030201f36e37ff

Concatenated: `fe000e0001c001021f02608201ab6b8201a7a061695fa0050d03000400a1563154a00e0c0c656e756d4d6170506172616da20302010aa503020103ad03020101af336831a00e670ca0050c034f6666a103020100a00e670ca0050c034c6f77a10302010aa00f670da0060c0448696768a103020114a0446942a0050d03000401a1393137a0100c0e74696c6465456e756d506172616da203020100a503020103a7140c125265640a7e52657365727665640a426c7565ad03020106a0426940a0050d03000402a1373135a00c0c0a6761696e536c69646572a203020100a3030201c4a40302010ca503020103a6070c052564206442a803020101ad03020101a0446942a0050d03000403a1393137a00d0c0b6c6576656c536c69646572a20b090980fddf10000000000000a3020900a40b0909800010000000000000a503020103ad03020102a0216a1fa0050d03000404a1163114a00d0c0b6f66666c696e654e6f6465a303010100a031692fa0050d03000405a1263124a00e0c0c6f66666c696e65506172616da203020107a503020101a903010100ad03020101a01c6a1aa0050d03000406a111310fa0080c066d6574657273a3030101fddf062afffe000e0001c001021f02603c653aa0146512a003020101a10b0909c004120ccccccccccda0146512a003020102a10b0909c0041568f5c28f5c29a00c650aa003020103a1030201eeaec5fffe000e0001c001021f02603c653aa0146512a003020101a10b0909c0041ab851eb851eb8a0146512a003020102a10b0909c0031a99999999999aa00c650aa003020103a1030201f36e37ff`

### Decoded BER tag breakdown

```
frame[0] BER:
  [APP 0] {
    [APP 11] {
      [0] {
        [APP 9] {
          [0] {
            RELATIVE-OID = 0x000400
          }
          [1] {
            SET {
              [0] {
                UTF8String = "enumMapParam"
              }
              [2] {
                INTEGER = 10
              }
              [5] {
                INTEGER = 3
              }
              [13] {
                INTEGER = 1
              }
              [15] {
                [APP 8] {
                  [0] {
                    [APP 7] {
                      [0] {
                        UTF8String = "Off"
                      }
                      [1] {
                        INTEGER = 0
                      }
                    }
                  }
                  [0] {
                    [APP 7] {
                      [0] {
                        UTF8String = "Low"
                      }
                      [1] {
                        INTEGER = 10
                      }
                    }
                  }
                  [0] {
                    [APP 7] {
                      [0] {
                        UTF8String = "High"
                      }
                      [1] {
                        INTEGER = 20
                      }
                    }
                  }
                }
              }
            }
          }
        }
      }
      [0] {
        [APP 9] {
          [0] {
            RELATIVE-OID = 0x000401
          }
          [1] {
            SET {
              [0] {
                UTF8String = "tildeEnumParam"
              }
              [2] {
                INTEGER = 0
              }
              [5] {
                INTEGER = 3
              }
              [7] {
                UTF8String = "Red
~Reserved
Blue"
              }
              [13] {
                INTEGER = 6
              }
            }
          }
        }
      }
      [0] {
        [APP 9] {
          [0] {
            RELATIVE-OID = 0x000402
          }
          [1] {
            SET {
              [0] {
                UTF8String = "gainSlider"
              }
              [2] {
                INTEGER = 0
              }
              [3] {
                INTEGER = -60
              }
              [4] {
                INTEGER = 12
              }
              [5] {
                INTEGER = 3
              }
              [6] {
                UTF8String = "%d dB"
              }
              [8] {
                INTEGER = 1
              }
              [13] {
                INTEGER = 1
              }
            }
          }
        }
      }
      [0] {
        [APP 9] {
          [0] {
            RELATIVE-OID = 0x000403
          }
          [1] {
            SET {
              [0] {
                UTF8String = "levelSlider"
              }
              [2] {
                REAL = 0.5
              }
              [3] {
                REAL = 0
              }
              [4] {
                REAL = 1
              }
              [5] {
                INTEGER = 3
              }
              [13] {
                INTEGER = 2
              }
            }
          }
        }
      }
      [0] {
        [APP 10] {
          [0] {
            RELATIVE-OID = 0x000404
          }
          [1] {
            SET {
              [0] {
                UTF8String = "offlineNode"
              }
              [3] {
                BOOLEAN = false
              }
            }
          }
        }
      }
      [0] {
        [APP 9] {
          [0] {
            RELATIVE-OID = 0x000405
          }
          [1] {
            SET {
              [0] {
                UTF8String = "offlineParam"
              }
              [2] {
                INTEGER = 7
              }
              [5] {
                INTEGER = 1
              }
              [9] {
                BOOLEAN = false
              }
              [13] {
                INTEGER = 1
              }
            }
          }
        }
      }
      [0] {
        [APP 10] {
          [0] {
            RELATIVE-OID = 0x000406
          }
          [1] {
            SET {
              [0] {
                UTF8String = "meters"
              }
              [3] {
                BOOLEAN = true
              }
            }
          }
        }
      }
    }
  }
frame[1] BER:
  [APP 0] {
    [APP 5] {
      [0] {
        [APP 5] {
          [0] {
            INTEGER = 1
          }
          [1] {
            REAL = -18.05
          }
        }
      }
      [0] {
        [APP 5] {
          [0] {
            INTEGER = 2
          }
          [1] {
            REAL = -21.41
          }
        }
      }
      [0] {
        [APP 5] {
          [0] {
            INTEGER = 3
          }
          [1] {
            INTEGER = -18
          }
        }
      }
    }
  }
frame[2] BER:
  [APP 0] {
    [APP 5] {
      [0] {
        [APP 5] {
          [0] {
            INTEGER = 1
          }
          [1] {
            REAL = -26.72
          }
        }
      }
      [0] {
        [APP 5] {
          [0] {
            INTEGER = 2
          }
          [1] {
            REAL = -13.3
          }
        }
      }
      [0] {
        [APP 5] {
          [0] {
            INTEGER = 3
          }
          [1] {
            INTEGER = -13
          }
        }
      }
    }
  }
```

### Decoded JSON (client.toJSON of interop subtree)

```json
{
  "nodeType": "QualifiedNode",
  "number": 4,
  "path": "0.4",
  "identifier": "interop",
  "isOnline": true,
  "children": [
    {
      "nodeType": "QualifiedParameter",
      "number": 0,
      "path": "0.4.0",
      "identifier": "enumMapParam",
      "value": 10,
      "access": "readWrite",
      "type": "integer",
      "enumMap": [
        {
          "key": "Off",
          "value": 0
        },
        {
          "key": "Low",
          "value": 10
        },
        {
          "key": "High",
          "value": 20
        }
      ]
    },
    {
      "nodeType": "QualifiedParameter",
      "number": 1,
      "path": "0.4.1",
      "identifier": "tildeEnumParam",
      "value": 0,
      "access": "readWrite",
      "enumeration": "Red\n~Reserved\nBlue",
      "type": "enum"
    },
    {
      "nodeType": "QualifiedParameter",
      "number": 2,
      "path": "0.4.2",
      "identifier": "gainSlider",
      "value": 0,
      "minimum": -60,
      "maximum": 12,
      "access": "readWrite",
      "format": "%d dB",
      "factor": 1,
      "type": "integer"
    },
    {
      "nodeType": "QualifiedParameter",
      "number": 3,
      "path": "0.4.3",
      "identifier": "levelSlider",
      "value": 0.5,
      "minimum": 0,
      "maximum": 1,
      "access": "readWrite",
      "type": "real"
    },
    {
      "nodeType": "QualifiedNode",
      "number": 4,
      "path": "0.4.4",
      "identifier": "offlineNode",
      "isOnline": false
    },
    {
      "nodeType": "QualifiedParameter",
      "number": 5,
      "path": "0.4.5",
      "identifier": "offlineParam",
      "value": 7,
      "access": "read",
      "isOnline": false,
      "type": "integer"
    },
    {
      "nodeType": "QualifiedNode",
      "number": 6,
      "path": "0.4.6",
      "identifier": "meters",
      "isOnline": true,
      "children": [
        {
          "nodeType": "QualifiedParameter",
          "number": 0,
          "path": "0.4.6.0",
          "identifier": "meterL",
          "value": 0,
          "minimum": -60,
          "maximum": 0,
          "access": "read",
          "type": "real",
          "streamIdentifier": 1
        },
        {
          "nodeType": "QualifiedParameter",
          "number": 1,
          "path": "0.4.6.1",
          "identifier": "meterR",
          "value": 0,
          "minimum": -60,
          "maximum": 0,
          "access": "read",
          "type": "real",
          "streamIdentifier": 2
        },
        {
          "nodeType": "QualifiedParameter",
          "number": 2,
          "path": "0.4.6.2",
          "identifier": "meterPeak",
          "value": 0,
          "minimum": -60,
          "maximum": 0,
          "access": "read",
          "type": "integer",
          "streamIdentifier": 3
        }
      ]
    }
  ]
}
```

## 2. meters getDirectory RESPONSE (node "meters", path 0.4.6)

Three streamed parameters (meterL id 1, meterR id 2, meterPeak id 3),
each carrying streamIdentifier [14].

- frame[0] (202 bytes):

      fe000e0001c001021f026081ba6b81b7a03c693aa0060d0400040600a130312ea0080c066d657465724ca2020900a30b0909c0051e000000000000a4020900a503020101ad03020102ae03020101a03c693aa0060d0400040601a130312ea0080c066d6574657252a2020900a30b0909c0051e000000000000a4020900a503020101ad03020102ae03020102a0396937a0060d0400040602a12d312ba00b0c096d657465725065616ba203020100a3030201c4a403020100a503020101ad03020101ae0302010319afff

- frame[1] (76 bytes):

      fe000e0001c001021f02603c653aa0146512a003020101a10b0909c00418547ae147ae14a0146512a003020102a10b0909c0021ab851eb851eb8a00c650aa003020103a1030201fdd9f325ff

- frame[2] (76 bytes):

      fe000e0001c001021f02603c653aa0146512a003020101a10b0909c0031fbd70a3d70a3da0146512a003020102a10b0909c00111333333333333a00c650aa003020103a1030201fdde2037ff

Concatenated: `fe000e0001c001021f026081ba6b81b7a03c693aa0060d0400040600a130312ea0080c066d657465724ca2020900a30b0909c0051e000000000000a4020900a503020101ad03020102ae03020101a03c693aa0060d0400040601a130312ea0080c066d6574657252a2020900a30b0909c0051e000000000000a4020900a503020101ad03020102ae03020102a0396937a0060d0400040602a12d312ba00b0c096d657465725065616ba203020100a3030201c4a403020100a503020101ad03020101ae0302010319affffe000e0001c001021f02603c653aa0146512a003020101a10b0909c00418547ae147ae14a0146512a003020102a10b0909c0021ab851eb851eb8a00c650aa003020103a1030201fdd9f325fffe000e0001c001021f02603c653aa0146512a003020101a10b0909c0031fbd70a3d70a3da0146512a003020102a10b0909c00111333333333333a00c650aa003020103a1030201fdde2037ff`

### Decoded BER tag breakdown

```
frame[0] BER:
  [APP 0] {
    [APP 11] {
      [0] {
        [APP 9] {
          [0] {
            RELATIVE-OID = 0x00040600
          }
          [1] {
            SET {
              [0] {
                UTF8String = "meterL"
              }
              [2] {
                REAL = 0
              }
              [3] {
                REAL = -60
              }
              [4] {
                REAL = 0
              }
              [5] {
                INTEGER = 1
              }
              [13] {
                INTEGER = 2
              }
              [14] {
                INTEGER = 1
              }
            }
          }
        }
      }
      [0] {
        [APP 9] {
          [0] {
            RELATIVE-OID = 0x00040601
          }
          [1] {
            SET {
              [0] {
                UTF8String = "meterR"
              }
              [2] {
                REAL = 0
              }
              [3] {
                REAL = -60
              }
              [4] {
                REAL = 0
              }
              [5] {
                INTEGER = 1
              }
              [13] {
                INTEGER = 2
              }
              [14] {
                INTEGER = 2
              }
            }
          }
        }
      }
      [0] {
        [APP 9] {
          [0] {
            RELATIVE-OID = 0x00040602
          }
          [1] {
            SET {
              [0] {
                UTF8String = "meterPeak"
              }
              [2] {
                INTEGER = 0
              }
              [3] {
                INTEGER = -60
              }
              [4] {
                INTEGER = 0
              }
              [5] {
                INTEGER = 1
              }
              [13] {
                INTEGER = 1
              }
              [14] {
                INTEGER = 3
              }
            }
          }
        }
      }
    }
  }
frame[1] BER:
  [APP 0] {
    [APP 5] {
      [0] {
        [APP 5] {
          [0] {
            INTEGER = 1
          }
          [1] {
            REAL = -24.33
          }
        }
      }
      [0] {
        [APP 5] {
          [0] {
            INTEGER = 2
          }
          [1] {
            REAL = -6.68
          }
        }
      }
      [0] {
        [APP 5] {
          [0] {
            INTEGER = 3
          }
          [1] {
            INTEGER = -7
          }
        }
      }
    }
  }
frame[2] BER:
  [APP 0] {
    [APP 5] {
      [0] {
        [APP 5] {
          [0] {
            INTEGER = 1
          }
          [1] {
            REAL = -15.87
          }
        }
      }
      [0] {
        [APP 5] {
          [0] {
            INTEGER = 2
          }
          [1] {
            REAL = -2.15
          }
        }
      }
      [0] {
        [APP 5] {
          [0] {
            INTEGER = 3
          }
          [1] {
            INTEGER = -2
          }
        }
      }
    }
  }
```

## 3. StreamCollection frame (provider push)

The provider has NO public broadcast API in node-emberplus, so it builds a bare
root TreeNode, attaches a StreamCollection via TreeNode.setStreams(), and queues it
to every connected client ~5x/sec. NOTE: in node-emberplus both StreamCollection and
StreamEntry use BERID = APPLICATION(5) (tag 0x65), and each StreamEntry encodes
streamIdentifier at [0] (context) and streamValue at [1] (context).

- frame[0] (76 bytes):

      fe000e0001c001021f02603c653aa0146512a003020101a10b0909c003115c28f5c28f5ca0146512a003020102a10b0909c0fddc1999999999999aa00c650aa003020103a1030201006a27ff

- frame[1] (77 bytes):

      fe000e0001c001021f02603c653aa0146512a003020101a10b0909c0011b333333333333a0146512a003020102a10b0909c0fddf170a3d70a3d70aa00c650aa003020103a1030201fddf810aff

- frame[2] (76 bytes):

      fe000e0001c001021f02603c653aa0146512a003020101a10b0909c0fdde1f5c28f5c28f5ca0146512a003020102a10b0909c0011fae147ae147aea00c650aa003020103a103020100c861ff

Concatenated (first frame): `fe000e0001c001021f02603c653aa0146512a003020101a10b0909c003115c28f5c28f5ca0146512a003020102a10b0909c0fddc1999999999999aa00c650aa003020103a1030201006a27ff`

### Decoded BER tag breakdown (first stream frame)

```
frame[0] BER:
  [APP 0] {
    [APP 5] {
      [0] {
        [APP 5] {
          [0] {
            INTEGER = 1
          }
          [1] {
            REAL = -8.68
          }
        }
      }
      [0] {
        [APP 5] {
          [0] {
            INTEGER = 2
          }
          [1] {
            REAL = -0.1
          }
        }
      }
      [0] {
        [APP 5] {
          [0] {
            INTEGER = 3
          }
          [1] {
            INTEGER = 0
          }
        }
      }
    }
  }
```

