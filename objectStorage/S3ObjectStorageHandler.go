package objectStorage

import (
	"log"
	"path"
	"reflect"
	"strings"
	"time"

	"github.com/aws/aws-sdk-go/aws"
	"github.com/aws/aws-sdk-go/aws/session"
	"github.com/aws/aws-sdk-go/service/s3"
)

type S3ObjectStorageHandler struct {
	S3Client *s3.S3
}

type UploadLinks struct {
	TSV             string `bakta:"tsv"`
	GFF3            string `bakta:"gff3"`
	GBFF            string `bakta:"gbff"`
	FNA             string `bakta:"fna"`
	FAA             string `bakta:"faa"`
	TSVHypothetical string `bakta:"hypotheticals.tsv"`
	FAAHypothetical string `bakta:"hypotheticals.faa"`
}

func InitS3ObjectStorageHandler(bucket string) (*S3ObjectStorageHandler, error) {
	s3Config := &aws.Config{
		Endpoint: aws.String("s3.computational.bio.uni-giessen.de"),
		Region:   aws.String("RegionOne"),
		//S3ForcePathStyle: aws.Bool(true),
	}

	sess := session.Must(session.NewSession(s3Config))
	s3Client := s3.New(sess)

	objectHandler := S3ObjectStorageHandler{
		S3Client: s3Client,
	}

	return &objectHandler, nil
}

func (handler *S3ObjectStorageHandler) CreateUploadLink(bucket string, key string) (string, error) {
	req, _ := handler.S3Client.PutObjectRequest(&s3.PutObjectInput{
		Bucket:      aws.String(bucket),
		Key:         aws.String(key),
		ContentType: aws.String("application/octet-stream"),
	})

	uploadURL, err := req.Presign(60 * time.Minute)
	if err != nil {
		log.Println(err.Error())
		return "", err
	}

	return uploadURL, nil
}

func (handler *S3ObjectStorageHandler) CreateDownloadLinks(bucket string, key string, prefix string) (*UploadLinks, error) {
	uploadLinks := UploadLinks{}

	uploadStructType := reflect.TypeOf(UploadLinks{})
	uploadStructValue := reflect.ValueOf(&uploadLinks)

	for i := 0; i < uploadStructType.NumField(); i++ {
		fieldFileSuffix := uploadStructType.Field(i).Tag.Get("bakta")
		fullFilename := strings.Join([]string{"result", ".", fieldFileSuffix}, "")

		keyWithFilename := path.Join(key, fullFilename)

		req, _ := handler.S3Client.GetObjectRequest(&s3.GetObjectInput{
			Bucket: aws.String(bucket),
			Key:    aws.String(keyWithFilename),
		})

		downloadURL, err := req.Presign(60 * time.Minute)
		if err != nil {
			log.Println(err.Error())
			return nil, err
		}
		uploadStructValueElem := uploadStructValue.Elem()
		uploadStructValueElem.Field(i).SetString(downloadURL)
	}

	return &uploadLinks, nil
}
