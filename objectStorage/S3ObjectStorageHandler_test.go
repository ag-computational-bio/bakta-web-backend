package objectStorage

import (
	"log"
	"testing"
)

func TestInitS3ObjectStorageHandler(t *testing.T) {
	handler, err := InitS3ObjectStorageHandler("baktadata")
	if err != nil {
		t.Fatal(err.Error())
	}

	url, err := handler.CreateUploadLink("baktadata", "test/data/foo.json")
	if err != nil {
		t.Fatal(err.Error())
	}

	log.Fatalln(url)
}
